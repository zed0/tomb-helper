use std::collections::HashMap;
use process_memory::{Architecture};

pub fn known_process_details() -> Vec<ProcessDetails> {
    vec![
        ProcessDetails::new("Tomb Raider 2013", "TombRaider.exe", Architecture::Arch32Bit, vec![
            (AddressType::XPosition, vec![0x00_F3_9E_18, 0x00]),
            (AddressType::YPosition, vec![0x00_F3_9E_18, 0x04]),
            (AddressType::ZPosition, vec![0x00_F3_9E_18, 0x08]),
        ].iter().cloned().collect()),
        ProcessDetails::new("Rise of the Tomb Raider", "ROTTR.exe", Architecture::Arch64Bit, vec![
            (AddressType::XPosition, vec![0x01_08_2A_B8, 0x10]),
            (AddressType::YPosition, vec![0x01_08_2A_B8, 0x14]),
            (AddressType::ZPosition, vec![0x01_08_2A_B8, 0x18]),
        ].iter().cloned().collect()),
        ProcessDetails::new("Shadow of the Tomb Raider", "SOTTR.exe", Architecture::Arch64Bit, vec![
            (AddressType::XPosition, vec![0x01_3D_5F_B8, 0x10]),
            (AddressType::YPosition, vec![0x01_3D_5F_B8, 0x14]),
            (AddressType::ZPosition, vec![0x01_3D_5F_B8, 0x18]),
        ].iter().cloned().collect()),
    ]
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum AddressType {
    XPosition,
    YPosition,
    ZPosition,
}

#[derive(Debug, Clone)]
pub struct ProcessDetails {
    pub name: String,
    pub executable_name: String,
    pub arch: Architecture,
    pub address_offsets: HashMap<AddressType, Vec<usize>>,
}

impl ProcessDetails {
    fn new(
        name: &str,
        executable_name: &str,
        arch: Architecture,
        address_offsets: HashMap<AddressType, Vec<usize>>
    ) -> ProcessDetails {
        ProcessDetails {
            name: String::from(name),
            executable_name: String::from(executable_name),
            arch,
            address_offsets,
        }
    }
}
