// Copyright 2022, The Android Open Source Project
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

//! VM bootloader example.

#![no_main]
#![no_std]
#![feature(default_alloc_error_handler)]

mod exceptions;

extern crate alloc;

use alloc::{vec, vec::Vec};
use buddy_system_allocator::LockedHeap;
use vmbase::{main, println};

static INITIALISED_DATA: [u32; 4] = [1, 2, 3, 4];
static mut ZEROED_DATA: [u32; 10] = [0; 10];
static mut MUTABLE_DATA: [u32; 4] = [1, 2, 3, 4];

#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap<32> = LockedHeap::<32>::new();

static mut HEAP: [u8; 65536] = [0; 65536];

main!(main);

/// Entry point for VM bootloader.
pub fn main() {
    println!("Hello world");
    print_addresses();
    check_data();

    unsafe {
        HEAP_ALLOCATOR.lock().init(&mut HEAP as *mut u8 as usize, HEAP.len());
    }

    check_alloc();
}

fn print_addresses() {
    unsafe {
        println!(
            "dtb:        {:#010x}-{:#010x} ({} bytes)",
            &dtb_begin as *const u8 as usize,
            &dtb_end as *const u8 as usize,
            &dtb_end as *const u8 as usize - &dtb_begin as *const u8 as usize,
        );
        println!(
            "text:       {:#010x}-{:#010x} ({} bytes)",
            &text_begin as *const u8 as usize,
            &text_end as *const u8 as usize,
            &text_end as *const u8 as usize - &text_begin as *const u8 as usize,
        );
        println!(
            "rodata:     {:#010x}-{:#010x} ({} bytes)",
            &rodata_begin as *const u8 as usize,
            &rodata_end as *const u8 as usize,
            &rodata_end as *const u8 as usize - &rodata_begin as *const u8 as usize,
        );
        println!(
            "data:       {:#010x}-{:#010x} ({} bytes, loaded at {:#010x})",
            &data_begin as *const u8 as usize,
            &data_end as *const u8 as usize,
            &data_end as *const u8 as usize - &data_begin as *const u8 as usize,
            &data_lma as *const u8 as usize,
        );
        println!(
            "bss:        {:#010x}-{:#010x} ({} bytes)",
            &bss_begin as *const u8 as usize,
            &bss_end as *const u8 as usize,
            &bss_end as *const u8 as usize - &bss_begin as *const u8 as usize,
        );
        println!(
            "boot_stack: {:#010x}-{:#010x} ({} bytes)",
            &boot_stack_begin as *const u8 as usize,
            &boot_stack_end as *const u8 as usize,
            &boot_stack_end as *const u8 as usize - &boot_stack_begin as *const u8 as usize,
        );
    }
}

fn check_data() {
    println!("INITIALISED_DATA: {:#010x}", &INITIALISED_DATA as *const u32 as usize);
    unsafe {
        println!("ZEROED_DATA: {:#010x}", &ZEROED_DATA as *const u32 as usize);
        println!("MUTABLE_DATA: {:#010x}", &MUTABLE_DATA as *const u32 as usize);
        println!("HEAP: {:#010x}", &HEAP as *const u8 as usize);
    }

    assert_eq!(INITIALISED_DATA[0], 1);
    assert_eq!(INITIALISED_DATA[1], 2);
    assert_eq!(INITIALISED_DATA[2], 3);
    assert_eq!(INITIALISED_DATA[3], 4);

    unsafe {
        for element in ZEROED_DATA.iter() {
            assert_eq!(*element, 0);
        }
        ZEROED_DATA[0] = 13;
        assert_eq!(ZEROED_DATA[0], 13);
        ZEROED_DATA[0] = 0;
        assert_eq!(ZEROED_DATA[0], 0);

        assert_eq!(MUTABLE_DATA[0], 1);
        assert_eq!(MUTABLE_DATA[1], 2);
        assert_eq!(MUTABLE_DATA[2], 3);
        assert_eq!(MUTABLE_DATA[3], 4);
        MUTABLE_DATA[0] += 41;
        assert_eq!(MUTABLE_DATA[0], 42);
    }
    println!("Data looks good");
}

fn check_alloc() {
    println!("Allocating a Vec...");
    let mut vector: Vec<u32> = vec![1, 2, 3, 4];
    assert_eq!(vector[0], 1);
    assert_eq!(vector[1], 2);
    assert_eq!(vector[2], 3);
    assert_eq!(vector[3], 4);
    vector[2] = 42;
    assert_eq!(vector[2], 42);
    println!("Vec seems to work.");
}

extern "C" {
    static dtb_begin: u8;
    static dtb_end: u8;
    static text_begin: u8;
    static text_end: u8;
    static rodata_begin: u8;
    static rodata_end: u8;
    static data_begin: u8;
    static data_end: u8;
    static data_lma: u8;
    static bss_begin: u8;
    static bss_end: u8;
    static boot_stack_begin: u8;
    static boot_stack_end: u8;
}
