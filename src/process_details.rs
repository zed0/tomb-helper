use process_memory::Architecture;
use std::collections::HashMap;

pub fn known_process_details() -> Vec<ProcessDetails> {
    vec![
        ProcessDetails::new(
            "Tomb Raider 2013",
            "TombRaider.exe",
            "v1.01.748.0",
            Architecture::Arch32Bit,
            vec![
                (AddressType::Version, vec![0x01_0E_65_98]),
                (AddressType::XPosition, vec![0x00_F3_9E_18, 0x00]),
                (AddressType::YPosition, vec![0x00_F3_9E_18, 0x04]),
                (AddressType::ZPosition, vec![0x00_F3_9E_18, 0x08]),
                (AddressType::CameraSin, vec![0x02_11_D6_98, 0x50]),
                (AddressType::CameraCos, vec![0x02_11_D6_98, 0x54]),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Tomb Raider 2013",
            "TombRaider.exe",
            "v1.1 build 838.0_32",
            Architecture::Arch32Bit,
            vec![
                (AddressType::Version, vec![0x01_DD_BB_F4, 0xF0]),
                (AddressType::XPosition, vec![0x00_A7_88_F0, 0x00]),
                (AddressType::YPosition, vec![0x00_A7_88_F0, 0x04]),
                (AddressType::ZPosition, vec![0x00_A7_88_F0, 0x08]),
                (AddressType::CameraSin, vec![0x00_15_0A_BC, 0x4_C4]),
                (AddressType::CameraCos, vec![0x00_15_0A_BC, 0x4_C8]),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Tomb Raider 2013",
            "TombRaider.exe",
            "v1.01.0.0",
            Architecture::Arch32Bit,
            vec![
                (AddressType::Version, vec![0x01_07_C8_98]),
                (AddressType::XPosition, vec![0x00_A7_88_F0, 0x00]),
                (AddressType::YPosition, vec![0x00_A7_88_F0, 0x04]),
                (AddressType::ZPosition, vec![0x00_A7_88_F0, 0x08]),
                (AddressType::CameraSin, vec![0x00_15_0C_8C, 0x4_C4]),
                (AddressType::CameraCos, vec![0x00_15_0C_8C, 0x4_C8]),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Rise of the Tomb Raider",
            "ROTTR.exe",
            "v1.0 build 820.0_64",
            Architecture::Arch64Bit,
            vec![
                (AddressType::Version, vec![0x01_06_6B_80, 0x90, 0x0238, 0x0]),
                (AddressType::XPosition, vec![0x01_08_2A_B8, 0x10]),
                (AddressType::YPosition, vec![0x01_08_2A_B8, 0x14]),
                (AddressType::ZPosition, vec![0x01_08_2A_B8, 0x18]),
                (AddressType::XLookAt, vec![0x01_5A_4F_88, 0x0140, 0x80]),
                (AddressType::YLookAt, vec![0x01_5A_4F_88, 0x0140, 0x84]),
                (AddressType::ZLookAt, vec![0x01_5A_4F_88, 0x0140, 0x88]),
                (AddressType::CameraSin, vec![0x02_DA_22_20, 0x04_90]),
                (AddressType::CameraCos, vec![0x02_DA_22_20, 0x04_94]),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Rise of the Tomb Raider",
            "ROTTR.exe",
            "v1.0 build 1027.0_64",
            Architecture::Arch64Bit,
            vec![
                (AddressType::Version, vec![0x02_E7_85_F8, 0x1E0]),
                (AddressType::XPosition, vec![0x01_0F_C8_C8, 0x10]),
                (AddressType::YPosition, vec![0x01_0F_C8_C8, 0x14]),
                (AddressType::ZPosition, vec![0x01_0F_C8_C8, 0x18]),
                (AddressType::CameraSin, vec![0x02_E1_CC_98, 0x70]),
                (AddressType::CameraCos, vec![0x02_E1_CC_98, 0x74]),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Shadow of the Tomb Raider",
            "SOTTR.exe",
            "v1.0 build 234.1_64",
            Architecture::Arch64Bit,
            vec![
                (AddressType::Version, vec![0x03_62_1B_00, 0xD8, 0x0B70]),
                (AddressType::XPosition, vec![0x01_3D_5F_B8, 0x10]),
                (AddressType::YPosition, vec![0x01_3D_5F_B8, 0x14]),
                (AddressType::ZPosition, vec![0x01_3D_5F_B8, 0x18]),
                (
                    AddressType::CameraSin,
                    vec![0x03_60_79_A0, 0x0C_60, 0x00, 0x60],
                ),
                (
                    AddressType::CameraCos,
                    vec![0x03_60_79_A0, 0x0C_60, 0x00, 0x64],
                ),
                (AddressType::CutscenePrompt, vec![0x01_41_B8_C0, 0x10]),
                (AddressType::CutsceneStatus, vec![0x01_41_B8_C0, 0x129]),
                (AddressType::CutsceneTimeline, vec![0x01_41_B8_C0, 0x60]),
                (AddressType::CutsceneLength, vec![0x01_41_B8_A0, 0x00, 0x20]),
                (
                    AddressType::CutsceneId,
                    vec![0x03_60_E0_D0, 0x0, 0x120, 0x10, 0x1D4],
                ),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Shadow of the Tomb Raider",
            "SOTTR.exe",
            "v1.0 build 298.0_64",
            Architecture::Arch64Bit,
            vec![
                (AddressType::Version, vec![0x03_6B_7B_B0, 0xD8, 0x0B60]),
                (AddressType::XPosition, vec![0x02_FF_34_70]),
                (AddressType::YPosition, vec![0x02_FF_34_74]),
                (AddressType::ZPosition, vec![0x02_FF_34_78]),
                (AddressType::CutscenePrompt, vec![0x01_49_51_C0, 0x10]),
                (AddressType::CutsceneStatus, vec![0x01_49_51_C0, 0x129]),
                (AddressType::CutsceneTimeline, vec![0x01_49_51_C0, 0x60]),
                (AddressType::CutsceneLength, vec![0x01_49_51_C0, 0x48, 0x10, 0x08]),
                (
                    AddressType::CutsceneId,
                    vec![0x03_69_3F_10, 0x68, 0x02C0, 0xA8, 0x022C],
                ),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Shadow of the Tomb Raider",
            "SOTTR.exe",
            "v1.0 build 458.0_64",
            Architecture::Arch64Bit,
            vec![
                (AddressType::Version, vec![0x03_6C_1B_60, 0x108, 0x30, 0x30]),
                (AddressType::XPosition, vec![0x01_46_90_B8, 0x10]),
                (AddressType::YPosition, vec![0x01_46_90_B8, 0x14]),
                (AddressType::ZPosition, vec![0x01_46_90_B8, 0x18]),
                (
                    AddressType::CameraSin,
                    vec![0x03_6A_7A_00, 0x70],
                ),
                (
                    AddressType::CameraCos,
                    vec![0x03_6A_7A_00, 0x74],
                ),
                (AddressType::CutscenePrompt, vec![0x01_4A_F1_40, 0x10]),
                (AddressType::CutsceneStatus, vec![0x01_4A_F1_40, 0x129]),
                (AddressType::CutsceneTimeline, vec![0x01_4A_F1_40, 0x60]),
                (AddressType::CutsceneLength, vec![0x01_4A_F1_20, 0x00, 0x20]),
                (
                    AddressType::CutsceneId,
                    vec![0x03_6A_E1_30, 0x00, 0x120, 0x10, 0x334],
                ),
            ]
            .iter()
            .cloned()
            .collect(),
        ),
        ProcessDetails::new(
            "Shadow of the Tomb Raider",
            "SOTTR.exe",
            "v1.0 build 489.0_64",
            Architecture::Arch64Bit,
            vec![
                (AddressType::Version, vec![0x03_6C_5B_40, 0x108, 0x258, 0x0]),
                (AddressType::XPosition, vec![0x01_46_D0_B8, 0x10]),
                (AddressType::YPosition, vec![0x01_46_D0_B8, 0x14]),
                (AddressType::ZPosition, vec![0x01_46_D0_B8, 0x18]),
                (
                    AddressType::CameraSin,
                    vec![0x03_6A_BA_00, 0x70],
                ),
                (
                    AddressType::CameraCos,
                    vec![0x03_6A_BA_00, 0x74],
                ),
                (AddressType::CutscenePrompt, vec![0x01_4B_31_40, 0x10]),
                (AddressType::CutsceneStatus, vec![0x01_4B_31_40, 0x129]),
                (AddressType::CutsceneTimeline, vec![0x01_4B_31_40, 0x60]),
                (AddressType::CutsceneLength, vec![0x01_4B_31_20, 0x00, 0x20]),
                (
                    AddressType::CutsceneId,
                    vec![0x03_6B_21_10, 0x00, 0x120, 0x10, 0x334],
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
    Version,
    XPosition,
    YPosition,
    ZPosition,
    XLookAt,
    YLookAt,
    ZLookAt,
    // TODO: Work out a more sensible way of representing the camera matrix
    // Note: The easiest way to find these values is to search for camera XYZ values which form the
    // last entries of the extrinsic camera matrix (https://ksimek.github.io/2012/08/22/extrinsic/)
    // From there the first 2 entries of the matrix can be used as CameraSin and CameraCos
    CameraSin,
    CameraCos,
    CutscenePrompt,
    CutsceneStatus,
    CutsceneTimeline,
    CutsceneLength,
    CutsceneId,
}

pub type AddressOffsets = HashMap<AddressType, Vec<usize>>;

#[derive(Debug, Clone)]
pub struct ProcessDetails {
    pub name: String,
    pub executable_name: String,
    pub version: String,
    pub arch: Architecture,
    pub address_offsets: AddressOffsets,
}

impl ProcessDetails {
    fn new(
        name: &str,
        executable_name: &str,
        version: &str,
        arch: Architecture,
        address_offsets: HashMap<AddressType, Vec<usize>>,
    ) -> ProcessDetails {
        ProcessDetails {
            name: String::from(name),
            executable_name: String::from(executable_name),
            version: String::from(version),
            arch,
            address_offsets,
        }
    }
}
