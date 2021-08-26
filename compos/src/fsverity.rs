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

use anyhow::{bail, Result};
use libc::getxattr;
use std::ffi::CString;
use std::io;
use std::os::unix::io::RawFd;

/// Magic used in fs-verity digest
const FS_VERITY_MAGIC: &[u8; 8] = b"FSVerity";

/// Hash algorithm to use from linux/fsverity.h
const FS_VERITY_HASH_ALG_SHA256: u8 = 1;

const SHA256_HASH_SIZE: usize = 32;

/// Size of `struct fsverity_formatted_digest` with SHA-256 in bytes.
const FORMATTED_SHA256_DIGEST_SIZE: usize = 12 + SHA256_HASH_SIZE;

/// Bytes of `struct fsverity_formatted_digest` in Linux with SHA-256.
pub type FormattedSha256Digest = [u8; FORMATTED_SHA256_DIGEST_SIZE];

/// Bytes of SHA256 digest
pub type Sha256Digest = [u8; SHA256_HASH_SIZE];

/// Returns the fs-verity measurement/digest. Currently only SHA256 is supported.
pub fn measure(fd: RawFd) -> Result<Sha256Digest> {
    // TODO(b/196635431): Unfortunately, the FUSE API doesn't allow authfs to implement the standard
    // fs-verity ioctls. Until the kernel allows, use the alternative xattr that authfs provides.
    let path = CString::new(format!("/proc/self/fd/{}", fd).as_str()).unwrap();
    let name = CString::new("authfs.fsverity.digest").unwrap();
    let mut buf = [0u8; SHA256_HASH_SIZE];
    // SAFETY: getxattr should not write beyond the given buffer size.
    let size = unsafe {
        getxattr(path.as_ptr(), name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len())
    };
    if size < 0 {
        bail!("Failed to getxattr: {}", io::Error::last_os_error());
    } else if size != SHA256_HASH_SIZE as isize {
        bail!("Unexpected hash size: {}", size);
    } else {
        Ok(buf)
    }
}

pub fn to_formatted_digest(digest: &Sha256Digest) -> FormattedSha256Digest {
    let mut formatted_digest: FormattedSha256Digest = [0; FORMATTED_SHA256_DIGEST_SIZE];
    formatted_digest[0..8].copy_from_slice(FS_VERITY_MAGIC);
    formatted_digest[8..10].copy_from_slice(&(FS_VERITY_HASH_ALG_SHA256 as u16).to_le_bytes());
    formatted_digest[10..12].copy_from_slice(&(SHA256_HASH_SIZE as u16).to_le_bytes());
    formatted_digest[12..].copy_from_slice(digest);
    formatted_digest
}
