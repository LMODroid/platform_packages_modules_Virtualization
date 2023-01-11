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

use crate::timeouts::TIMEOUTS;
use crate::{
    get_vm_config_path, BUILD_MANIFEST_APK_PATH, BUILD_MANIFEST_SYSTEM_EXT_APK_PATH,
    COMPOS_APEX_ROOT, COMPOS_DATA_ROOT, COMPOS_VSOCK_PORT,
};
use android_system_virtualizationservice::aidl::android::system::virtualizationservice::{
    IVirtualizationService::IVirtualizationService,
    VirtualMachineAppConfig::{DebugLevel::DebugLevel, Payload::Payload, VirtualMachineAppConfig},
    VirtualMachineConfig::VirtualMachineConfig,
};
use anyhow::{bail, Context, Result};
use binder::{ParcelFileDescriptor, Strong};
use compos_aidl_interface::aidl::com::android::compos::ICompOsService::ICompOsService;
use log::{info, warn};
use rustutils::system_properties;
use std::fs::{self, File};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use vmclient::{DeathReason, ErrorCode, VmInstance, VmWaitError};

/// This owns an instance of the CompOS VM.
pub struct ComposClient(VmInstance);

/// Parameters to be used when creating a virtual machine instance.
#[derive(Default, Debug, Clone)]
pub struct VmParameters {
    /// Whether the VM should be debuggable.
    pub debug_mode: bool,
    /// Number of vCPUs to have in the VM. If None, defaults to 1.
    pub cpus: Option<NonZeroU32>,
    /// List of task profiles to apply to the VM
    pub task_profiles: Vec<String>,
    /// If present, overrides the amount of RAM to give the VM
    pub memory_mib: Option<i32>,
    /// Whether the VM prefers staged APEXes or activated ones (false; default)
    pub prefer_staged: bool,
}

impl ComposClient {
    /// Start a new CompOS VM instance using the specified instance image file and parameters.
    pub fn start(
        service: &dyn IVirtualizationService,
        instance_image: File,
        idsig: &Path,
        idsig_manifest_apk: &Path,
        idsig_manifest_ext_apk: &Path,
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

        let manifest_apk_fd = File::open(BUILD_MANIFEST_APK_PATH)
            .context("Failed to open build manifest APK file")?;
        let manifest_apk_fd = ParcelFileDescriptor::new(manifest_apk_fd);
        let idsig_manifest_apk_fd = prepare_idsig(service, &manifest_apk_fd, idsig_manifest_apk)?;

        // Prepare a few things based on whether /system_ext exists, including:
        // 1. generate the additional idsig FD for the APK from /system_ext, then pass to VS
        // 2. select the correct VM config json
        let (extra_idsigs, has_system_ext) =
            if let Ok(manifest_ext_apk_fd) = File::open(BUILD_MANIFEST_SYSTEM_EXT_APK_PATH) {
                // Optional idsig in /system_ext is found, so prepare additionally.
                let manifest_ext_apk_fd = ParcelFileDescriptor::new(manifest_ext_apk_fd);
                let idsig_manifest_ext_apk_fd =
                    prepare_idsig(service, &manifest_ext_apk_fd, idsig_manifest_ext_apk)?;

                (vec![idsig_manifest_apk_fd, idsig_manifest_ext_apk_fd], true)
            } else {
                (vec![idsig_manifest_apk_fd], false)
            };
        let config_path = get_vm_config_path(has_system_ext, parameters.prefer_staged);

        let debug_level = if parameters.debug_mode { DebugLevel::FULL } else { DebugLevel::NONE };

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

        let config = VirtualMachineConfig::AppConfig(VirtualMachineAppConfig {
            name: String::from("Compos"),
            apk: Some(apk_fd),
            idsig: Some(idsig_fd),
            instanceImage: Some(instance_fd),
            encryptedStorageImage: None,
            payload: Payload::ConfigPath(config_path),
            debugLevel: debug_level,
            extraIdsigs: extra_idsigs,
            protectedVm: protected_vm,
            memoryMib: parameters.memory_mib.unwrap_or(0), // 0 means use the default
            numCpus: parameters.cpus.map_or(1, NonZeroU32::get) as i32,
            taskProfiles: parameters.task_profiles.clone(),
        });

        let callback = Box::new(Callback {});
        let instance = VmInstance::create(service, &config, console_fd, log_fd, Some(callback))
            .context("Failed to create VM")?;

        instance.start()?;

        let ready = instance.wait_until_ready(TIMEOUTS.vm_max_time_to_ready);
        if ready == Err(VmWaitError::Finished) && debug_level != DebugLevel::NONE {
            // The payload has (unexpectedly) finished, but the VM is still running. Give it
            // some time to shutdown to maximize our chances of getting useful logs.
            if let Some(death_reason) =
                instance.wait_for_death_with_timeout(TIMEOUTS.vm_max_time_to_exit)
            {
                bail!("VM died during startup - reason {:?}", death_reason);
            }
        }
        ready?;

        Ok(Self(instance))
    }

    /// Create and return an RPC Binder connection to the Comp OS service in the VM.
    pub fn connect_service(&self) -> Result<Strong<dyn ICompOsService>> {
        self.0.connect_service(COMPOS_VSOCK_PORT).context("Connecting to CompOS service")
    }

    /// Shut down the VM cleanly, by sending a quit request to the service, giving time for any
    /// relevant logs to be written.
    pub fn shutdown(self, service: Strong<dyn ICompOsService>) {
        info!("Requesting CompOS VM to shutdown");
        let _ = service.quit(); // If this fails, the VM is probably dying anyway
        self.wait_for_shutdown();
    }

    /// Wait for the instance to shut down. If it fails to shutdown within a reasonable time the
    /// instance is dropped, which forcibly terminates it.
    /// This should only be called when the instance has been requested to quit, or we believe that
    /// it is already in the process of exiting due to some failure.
    fn wait_for_shutdown(self) {
        let death_reason = self.0.wait_for_death_with_timeout(TIMEOUTS.vm_max_time_to_exit);
        match death_reason {
            Some(DeathReason::Shutdown) => info!("VM has exited normally"),
            Some(reason) => warn!("VM died with reason {:?}", reason),
            None => warn!("VM failed to exit, dropping"),
        }
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

    let have_non_protected_vm =
        system_properties::read_bool("ro.boot.hypervisor.vm.supported", false)?;
    if have_non_protected_vm {
        warn!("Protected VM not supported, falling back to non-protected on debuggable build");
        return Ok(false);
    }

    bail!("No VM support available")
}

struct Callback {}
impl vmclient::VmCallback for Callback {
    fn on_payload_started(&self, cid: i32) {
        log::info!("VM payload started, cid = {}", cid);
    }

    fn on_payload_ready(&self, cid: i32) {
        log::info!("VM payload ready, cid = {}", cid);
    }

    fn on_payload_finished(&self, cid: i32, exit_code: i32) {
        log::warn!("VM payload finished, cid = {}, exit code = {}", cid, exit_code);
    }

    fn on_error(&self, cid: i32, error_code: ErrorCode, message: &str) {
        log::warn!("VM error, cid = {}, error code = {:?}, message = {}", cid, error_code, message);
    }

    fn on_died(&self, cid: i32, death_reason: DeathReason) {
        log::warn!("VM died, cid = {}, reason = {:?}", cid, death_reason);
    }
}
