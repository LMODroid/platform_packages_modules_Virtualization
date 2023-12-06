// Copyright 2023, The Android Open Source Project
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! This library contains the communication protocol used between the host
//! and the service VM.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod csr;
mod message;
mod vsock;

pub use csr::{cbor_value_type, to_unexpected_item_error, value_to_bytes, Csr, CsrPayload};
pub use message::{
    ClientVmAttestationParams, EcdsaP256KeyPair, GenerateCertificateRequestParams, Request,
    RequestProcessingError, Response, ServiceVmRequest,
};
pub use vsock::VmType;
