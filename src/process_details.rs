use process_memory::Architecture;
use std::collections::HashMap;

pub fn known_process_details() -> Vec<ProcessDetails> {
    vec![
        ProcessDetails::new(
            "Tomb Raider 2013",
            "TombRaider.exe",
            Architecture::Arch32Bit,
            vec![
                (AddressType::XPosition, vec![0x00_F3_9E_18, 0x00]),
                (AddressType::YPosition, vec![0x00_F3_9E_18, 0x04]),
                (AddressType::ZPosition, vec![0x00_F3_9E_18, 0x08]),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Rise of the Tomb Raider",
            "ROTTR.exe",
            Architecture::Arch64Bit,
            vec![
                (AddressType::XPosition, vec![0x01_08_2A_B8, 0x10]),
                (AddressType::YPosition, vec![0x01_08_2A_B8, 0x14]),
                (AddressType::ZPosition, vec![0x01_08_2A_B8, 0x18]),
                (AddressType::XLookAt, vec![0x01_5A_4F_88, 0x0140, 0x80]),
                (AddressType::YLookAt, vec![0x01_5A_4F_88, 0x0140, 0x84]),
                (AddressType::ZLookAt, vec![0x01_5A_4F_88, 0x0140, 0x88]),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Shadow of the Tomb Raider",
            "SOTTR.exe",
            Architecture::Arch64Bit,
            vec![
                (AddressType::XPosition, vec![0x01_3D_5F_B8, 0x10]),
                (AddressType::YPosition, vec![0x01_3D_5F_B8, 0x14]),
                (AddressType::ZPosition, vec![0x01_3D_5F_B8, 0x18]),
                (AddressType::CutscenePrompt, vec![0x01_41_B8_C0, 0x10]),
                (AddressType::CutsceneStatus, vec![0x01_41_B8_C0, 0x129]),
                (AddressType::CutsceneTimeline, vec![0x01_41_B8_C0, 0x60]),
                (
                    AddressType::CutsceneId,
                    vec![0x03_60_E0_D0, 0x0, 0x120, 0x10, 0x1D4],
                ),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
    ]
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum AddressType {
    XPosition,
    YPosition,
    ZPosition,
    XLookAt,
    YLookAt,
    ZLookAt,
    CutscenePrompt,
    CutsceneStatus,
    CutsceneTimeline,
    CutsceneId,
}

pub type AddressOffsets = HashMap<AddressType, Vec<usize>>;

#[derive(Debug, Clone)]
pub struct ProcessDetails {
    pub name: String,
    pub executable_name: String,
    pub arch: Architecture,
    pub address_offsets: AddressOffsets,
}

impl ProcessDetails {
    fn new(
        name: &str,
        executable_name: &str,
        arch: Architecture,
        address_offsets: HashMap<AddressType, Vec<usize>>,
    ) -> ProcessDetails {
        ProcessDetails {
            name: String::from(name),
            executable_name: String::from(executable_name),
            arch,
            address_offsets,
        }
    }
}
