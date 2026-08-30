use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileIdentity {
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareInventory {
    pub root: PathBuf,
    pub mcpx: PathBuf,
    pub flash: PathBuf,
    pub hdd_template: PathBuf,
    pub eeprom_template: Option<PathBuf>,
    pub mcpx_identity: FileIdentity,
    pub flash_identity: FileIdentity,
    pub hdd_template_identity: FileIdentity,
    pub eeprom_template_identity: Option<FileIdentity>,
}

fn managed_firmware_root() -> PathBuf {
    super::super::emu_home_base().join("firmware/xemu")
}

fn fallback_firmware_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("workspace/retrobios/bios/Microsoft/Xbox"))
}

pub fn resolve_firmware() -> io::Result<FirmwareInventory> {
    if let Some(explicit) = std::env::var_os("EMUCAP_XEMU_FIRMWARE") {
        let root = PathBuf::from(explicit);
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "EMUCAP_XEMU_FIRMWARE must be an absolute directory",
            ));
        }
        return validate_firmware_root(&root);
    }
    let mut candidates = vec![managed_firmware_root()];
    if let Some(path) = fallback_firmware_root() {
        candidates.push(path);
    }
    for root in &candidates {
        if root.is_dir()
            && ["mcpx_1.0.bin", "Complex_4627.bin", "xbox_hdd.qcow2"]
                .iter()
                .all(|name| root.join(name).exists())
        {
            return validate_firmware_root(root);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "original Xbox firmware inventory is unavailable; set EMUCAP_XEMU_FIRMWARE to an absolute directory containing mcpx_1.0.bin, Complex_4627.bin, and xbox_hdd.qcow2",
    ))
}

pub fn validate_firmware_root(root: &Path) -> io::Result<FirmwareInventory> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "xemu firmware inventory must be a plain directory: {}",
                root.display()
            ),
        ));
    }
    let mcpx = root.join("mcpx_1.0.bin");
    let flash = root.join("Complex_4627.bin");
    let hdd_template = root.join("xbox_hdd.qcow2");
    let eeprom_candidate = root.join("xemu_eeprom.bin");
    let mcpx_identity = identity_for_regular_file(&mcpx)?;
    if mcpx_identity.size != 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "xemu MCPX must be exactly 512 bytes, got {}",
                mcpx_identity.size
            ),
        ));
    }
    let flash_identity = identity_for_regular_file(&flash)?;
    if ![256 * 1024, 512 * 1024, 1024 * 1024].contains(&flash_identity.size) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "xemu flash ROM must be 256 KiB, 512 KiB, or 1 MiB, got {} bytes",
                flash_identity.size
            ),
        ));
    }
    let hdd_template_identity = identity_for_regular_file(&hdd_template)?;
    let (mut hdd, _) = open_regular_no_follow(&hdd_template)?;
    let mut magic = [0_u8; 4];
    hdd.read_exact(&mut magic)?;
    if magic != *b"QFI\xfb" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "xemu HDD template is not a QCOW image",
        ));
    }
    let (eeprom_template, eeprom_template_identity) = match fs::symlink_metadata(&eeprom_candidate)
    {
        Ok(_) => {
            let identity = identity_for_regular_file(&eeprom_candidate)?;
            if identity.size != 256 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "xemu EEPROM template must be 256 bytes, got {}",
                        identity.size
                    ),
                ));
            }
            (Some(eeprom_candidate), Some(identity))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, None),
        Err(error) => return Err(error),
    };
    Ok(FirmwareInventory {
        root: root.to_path_buf(),
        mcpx,
        flash,
        hdd_template,
        eeprom_template,
        mcpx_identity,
        flash_identity,
        hdd_template_identity,
        eeprom_template_identity,
    })
}

fn open_regular_no_follow(path: &Path) -> io::Result<(fs::File, fs::Metadata)> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("expected a regular non-symlink file: {}", path.display()),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() != before.len() {
        return Err(io::Error::other(format!(
            "file changed while opening: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != before.dev() || opened.ino() != before.ino() {
            return Err(io::Error::other(format!(
                "file identity changed while opening: {}",
                path.display()
            )));
        }
    }
    Ok((file, opened))
}

fn hash_reader(mut reader: impl Read) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    let mut size = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("file size overflow while hashing"))?;
        hasher.update(&buffer[..count]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

pub fn identity_for_regular_file(path: &Path) -> io::Result<FileIdentity> {
    let (file, metadata) = open_regular_no_follow(path)?;
    let (size, sha256) = hash_reader(file)?;
    if size != metadata.len() {
        return Err(io::Error::other(format!(
            "file changed while hashing: {}",
            path.display()
        )));
    }
    Ok(FileIdentity { size, sha256 })
}

pub(super) fn copy_verified(
    source: &Path,
    destination: &Path,
    expected: &FileIdentity,
) -> io::Result<()> {
    let (mut input, metadata) = open_regular_no_follow(source)?;
    if metadata.len() != expected.size {
        return Err(io::Error::other(format!(
            "source changed before managed copy: {}",
            source.display()
        )));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        copied += count as u64;
    }
    output.flush()?;
    output.sync_all()?;
    let actual = hex::encode(hasher.finalize());
    if copied != expected.size || actual != expected.sha256 {
        let _ = fs::remove_file(destination);
        return Err(io::Error::other(format!(
            "source changed while creating managed copy: {}",
            source.display()
        )));
    }
    Ok(())
}
