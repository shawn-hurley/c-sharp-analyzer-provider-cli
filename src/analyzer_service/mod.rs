#![allow(dead_code)]
include!("provider.rs");

pub mod proto {
    pub(crate) const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("provider_service_descriptor.bin");
}
