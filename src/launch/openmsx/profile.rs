use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Cartridge,
    Disk,
    Cassette,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cartridge => "cartridge",
            Self::Disk => "disk",
            Self::Cassette => "cassette",
        }
    }

    pub fn command_switch(self) -> &'static str {
        match self {
            Self::Cartridge => "-cart",
            Self::Disk => "-diska",
            Self::Cassette => "-cassetteplayer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMsxProfile {
    CbiosMsx2p,
    Msx1,
    Msx2,
    Msx2p,
    MsxTurboR,
}

impl OpenMsxProfile {
    pub fn for_system(system: &str) -> Option<Self> {
        match system {
            "msx" => Some(Self::CbiosMsx2p),
            "msx1" => Some(Self::Msx1),
            "msx2" => Some(Self::Msx2),
            "msx2p" => Some(Self::Msx2p),
            "msxtr" => Some(Self::MsxTurboR),
            _ => None,
        }
    }

    pub fn system(self) -> &'static str {
        match self {
            Self::CbiosMsx2p => "msx",
            Self::Msx1 => "msx1",
            Self::Msx2 => "msx2",
            Self::Msx2p => "msx2p",
            Self::MsxTurboR => "msxtr",
        }
    }

    pub fn machine(self) -> &'static str {
        match self {
            Self::CbiosMsx2p => "C-BIOS_MSX2+",
            Self::Msx1 => "Philips_VG_8020",
            Self::Msx2 => "Philips_NMS_8250",
            Self::Msx2p => "Panasonic_FS-A1WSX",
            Self::MsxTurboR => "Panasonic_FS-A1GT",
        }
    }

    pub fn machine_type(self) -> &'static str {
        match self {
            Self::CbiosMsx2p | Self::Msx2p => "MSX2+",
            Self::Msx1 => "MSX",
            Self::Msx2 => "MSX2",
            Self::MsxTurboR => "MSXturboR",
        }
    }

    pub fn supports(self, media: MediaKind) -> bool {
        match self {
            Self::CbiosMsx2p => media == MediaKind::Cartridge,
            Self::Msx1 => matches!(media, MediaKind::Cartridge | MediaKind::Cassette),
            Self::Msx2 | Self::Msx2p => true,
            Self::MsxTurboR => matches!(media, MediaKind::Cartridge | MediaKind::Disk),
        }
    }

    pub fn expected_region_sizes(self) -> (u64, u64, u64) {
        match self {
            Self::CbiosMsx2p => (65_536, 512 * 1024, 128 * 1024),
            Self::Msx1 => (65_536, 64 * 1024, 128 * 1024),
            Self::Msx2 => (65_536, 128 * 1024, 128 * 1024),
            Self::Msx2p => (65_536, 64 * 1024, 128 * 1024),
            Self::MsxTurboR => (65_536, 512 * 1024, 128 * 1024),
        }
    }

    pub fn uses_real_firmware(self) -> bool {
        self != Self::CbiosMsx2p
    }
}

pub fn classify_media_extension(extension: &str) -> Option<MediaKind> {
    match extension.to_ascii_lowercase().as_str() {
        "rom" | "mx1" | "mx2" | "ri" | "sg" => Some(MediaKind::Cartridge),
        "dsk" => Some(MediaKind::Disk),
        "cas" | "tsx" | "wav" => Some(MediaKind::Cassette),
        _ => None,
    }
}
