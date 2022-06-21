/*
 * Copyright (C) 2021 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Support for starting CompOS in a VM and connecting to the service

use crate::timeouts::timeouts;
use crate::{COMPOS_APEX_ROOT, COMPOS_DATA_ROOT, COMPOS_VSOCK_PORT, DEFAULT_VM_CONFIG_PATH};
use android_system_virtualizationservice::aidl::android::system::virtualizationservice::{
    DeathReason::DeathReason,
    IVirtualMachineCallback::{BnVirtualMachineCallback, IVirtualMachineCallback},
    IVirtualizationService::IVirtualizationService,
    VirtualMachineAppConfig::{DebugLevel::DebugLevel, VirtualMachineAppConfig},
    VirtualMachineConfig::VirtualMachineConfig,
};
use android_system_virtualizationservice::binder::{
    BinderFeatures, Interface, ParcelFileDescriptor, Result as BinderResult, Strong,
};
use anyhow::{bail, Context, Result};
use compos_aidl_interface::aidl::com::android::compos::ICompOsService::ICompOsService;
use log::{info, warn};
use rustutils::system_properties;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::thread;
use vmclient::VmInstance;

/// This owns an instance of the CompOS VM.
pub struct ComposClient(VmInstance);

/// Parameters to be used when creating a virtual machine instance.
#[derive(Default, Debug, Clone)]
pub struct VmParameters {
    /// Whether the VM should be debuggable.
    pub debug_mode: bool,
    /// Number of vCPUs to have in the VM. If None, defaults to 1.
    pub cpus: Option<NonZeroU32>,
    /// Comma separated list of host CPUs where vCPUs are assigned to. If None, any host CPU can be
    /// used to run any vCPU.
    pub cpu_set: Option<String>,
    /// List of task profiles to apply to the VM
    pub task_profiles: Vec<String>,
    /// If present, overrides the path to the VM config JSON file
    pub config_path: Option<String>,
    /// If present, overrides the amount of RAM to give the VM
    pub memory_mib: Option<i32>,
}

impl ComposClient {
    /// Start a new CompOS VM instance using the specified instance image file and parameters.
    pub fn start(
        service: &dyn IVirtualizationService,
        instance_image: File,
        idsig: &Path,
        idsig_manifest_apk: &Path,
        parameters: &VmParameters,
    ) -> Result<Self> {
        let protected_vm = want_protected_vm()?;

        let instance_fd = ParcelFileDescriptor::new(instance_image);

        let apex_dir = Path::new(COMPOS_APEX_ROOT);
        let data_dir = Path::new(COMPOS_DATA_ROOT);

        let config_apk = locate_config_apk(apex_dir)?;
        let apk_fd = File::open(config_apk).context("Failed to open config APK file")?;
        let apk_fd = ParcelFileDescriptor::new(apk_fd);
        let idsig_fd = prepare_idsig(service, &apk_fd, idsig)?;

        let manifest_apk_fd = File::open("/system/etc/security/fsverity/BuildManifest.apk")
            .context("Failed to open build manifest APK file")?;
        let manifest_apk_fd = ParcelFileDescriptor::new(manifest_apk_fd);
        let idsig_manifest_apk_fd = prepare_idsig(service, &manifest_apk_fd, idsig_manifest_apk)?;

        let debug_level = match (protected_vm, parameters.debug_mode) {
            (_, true) => DebugLevel::FULL,
            (false, false) => DebugLevel::APP_ONLY,
            (true, false) => DebugLevel::NONE,
        };

        let (console_fd, log_fd) = if debug_level == DebugLevel::NONE {
            (None, None)
        } else {
            // Console output and the system log output from the VM are redirected to file.
            let console_fd = File::create(data_dir.join("vm_console.log"))
                .context("Failed to create console log file")?;
            let log_fd = File::create(data_dir.join("vm.log"))
                .context("Failed to create system log file")?;
            info!("Running in debug level {:?}", debug_level);
            (Some(console_fd), Some(log_fd))
        };

        let config_path = parameters.config_path.as_deref().unwrap_or(DEFAULT_VM_CONFIG_PATH);
        let config = VirtualMachineConfig::AppConfig(VirtualMachineAppConfig {
            apk: Some(apk_fd),
            idsig: Some(idsig_fd),
            instanceImage: Some(instance_fd),
            configPath: config_path.to_owned(),
            debugLevel: debug_level,
            extraIdsigs: vec![idsig_manifest_apk_fd],
            protectedVm: protected_vm,
            memoryMib: parameters.memory_mib.unwrap_or(0), // 0 means use the default
            numCpus: parameters.cpus.map_or(1, NonZeroU32::get) as i32,
            cpuAffinity: parameters.cpu_set.clone(),
            taskProfiles: parameters.task_profiles.clone(),
        });

        let instance = VmInstance::create(service, &config, console_fd, log_fd)
            .context("Failed to create VM")?;

        let callback =
            BnVirtualMachineCallback::new_binder(VmCallback(), BinderFeatures::default());
        instance.vm.registerCallback(&callback)?;

        instance.start()?;

        instance.wait_until_ready(timeouts()?.vm_max_time_to_ready)?;

        Ok(Self(instance))
    }

    /// Create and return an RPC Binder connection to the Comp OS service in the VM.
    pub fn get_service(&self) -> Result<Strong<dyn ICompOsService>> {
        self.0.get_service(COMPOS_VSOCK_PORT).context("Connecting to CompOS service")
    }
}

fn locate_config_apk(apex_dir: &Path) -> Result<PathBuf> {
    // Our config APK will be in a directory under app, but the name of the directory is at the
    // discretion of the build system. So just look in each sub-directory until we find it.
    // (In practice there will be exactly one directory, so this shouldn't take long.)
    let app_dir = apex_dir.join("app");
    for dir in fs::read_dir(app_dir).context("Reading app dir")? {
        let apk_file = dir?.path().join("CompOSPayloadApp.apk");
        if apk_file.is_file() {
            return Ok(apk_file);
        }
    }

    bail!("Failed to locate CompOSPayloadApp.apk")
}

fn prepare_idsig(
    service: &dyn IVirtualizationService,
    apk_fd: &ParcelFileDescriptor,
    idsig_path: &Path,
) -> Result<ParcelFileDescriptor> {
    if !idsig_path.exists() {
        // Prepare idsig file via VirtualizationService
        let idsig_file = File::create(idsig_path).context("Failed to create idsig file")?;
        let idsig_fd = ParcelFileDescriptor::new(idsig_file);
        service
            .createOrUpdateIdsigFile(apk_fd, &idsig_fd)
            .context("Failed to update idsig file")?;
    }

    // Open idsig as read-only
    let idsig_file = File::open(idsig_path).context("Failed to open idsig file")?;
    let idsig_fd = ParcelFileDescriptor::new(idsig_file);
    Ok(idsig_fd)
}

fn want_protected_vm() -> Result<bool> {
    let have_protected_vm =
        system_properties::read_bool("ro.boot.hypervisor.protected_vm.supported", false)?;
    if have_protected_vm {
        info!("Starting protected VM");
        return Ok(true);
    }

    let is_debug_build = system_properties::read("ro.debuggable")?.as_deref().unwrap_or("0") == "1";
    if !is_debug_build {
        bail!("Protected VM not supported, unable to start VM");
    }

    let have_unprotected_vm =
        system_properties::read_bool("ro.boot.hypervisor.vm.supported", false)?;
    if have_unprotected_vm {
        warn!("Protected VM not supported, falling back to unprotected on debuggable build");
        return Ok(false);
    }

    bail!("No VM support available")
}

struct VmCallback();

impl Interface for VmCallback {}

impl IVirtualMachineCallback for VmCallback {
    fn onDied(&self, cid: i32, reason: DeathReason) -> BinderResult<()> {
        log::warn!("VM died, cid = {}, reason = {:?}", cid, reason);
        Ok(())
    }

    fn onPayloadStarted(
        &self,
        cid: i32,
        stream: Option<&ParcelFileDescriptor>,
    ) -> BinderResult<()> {
        if let Some(pfd) = stream {
            if let Err(e) = start_logging(pfd) {
                warn!("Can't log vm output: {}", e);
            };
        }
        log::info!("VM payload started, cid = {}", cid);
        Ok(())
    }

    fn onPayloadReady(&self, cid: i32) -> BinderResult<()> {
        log::info!("VM payload ready, cid = {}", cid);
        Ok(())
    }

    fn onPayloadFinished(&self, cid: i32, exit_code: i32) -> BinderResult<()> {
        log::warn!("VM payload finished, cid = {}, exit code = {}", cid, exit_code);
        Ok(())
    }

    fn onError(&self, cid: i32, error_code: i32, message: &str) -> BinderResult<()> {
        log::warn!("VM error, cid = {}, error code = {}, message = {}", cid, error_code, message,);
        Ok(())
    }
}

fn start_logging(pfd: &ParcelFileDescriptor) -> Result<()> {
    let reader = BufReader::new(pfd.as_ref().try_clone().context("Cloning fd failed")?);
    thread::spawn(move || {
        for line in reader.lines() {
            match line {
                Ok(line) => info!("VM: {}", line),
                Err(e) => {
                    warn!("Reading VM output failed: {}", e);
                    break;
                }
            }
        }
    });
    Ok(())
}
