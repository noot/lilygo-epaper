use alloc::{format, string::String, vec::Vec};

use embedded_sdmmc::{
    DirEntry,
    LfnBuffer,
    Mode,
    SdCardError,
    ShortFileName,
    TimeSource,
    Timestamp,
    VolumeIdx,
    VolumeManager,
};
use esp_hal::{
    delay::Delay,
    spi::{
        master::{Config as SpiConfig, ConfigError as SpiConfigError},
        Mode as SpiMode,
    },
    time::Rate,
};

use crate::spi::{Bus, SharedSpiDevice};

type BlockDevice<'a, 'd> = embedded_sdmmc::SdCard<SharedSpiDevice<'a, 'd>, Delay>;
type VolumeManagerType<'a, 'd> = VolumeManager<BlockDevice<'a, 'd>, SdTimeSource, 4, 4, 1>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub is_directory: bool,
    pub size: u32,
}

pub type Metadata = DirectoryEntry;

#[derive(Clone, Copy, Debug)]
pub struct SdTimeSource {
    timestamp: Timestamp,
}

impl SdTimeSource {
    pub fn new(timestamp: Timestamp) -> Self {
        Self { timestamp }
    }
}

impl Default for SdTimeSource {
    fn default() -> Self {
        Self {
            timestamp: Timestamp::from_calendar(2026, 1, 1, 0, 0, 0)
                .expect("calendar literal is valid"),
        }
    }
}

impl TimeSource for SdTimeSource {
    fn get_timestamp(&self) -> Timestamp {
        self.timestamp
    }
}

#[derive(Debug)]
pub enum Error {
    /// SPI bus configuration failed.
    SpiConfig(SpiConfigError),
    /// SPI transfer failed.
    Spi(esp_hal::spi::Error),
    /// Filesystem operation failed.
    Filesystem(embedded_sdmmc::Error<SdCardError>),
    /// SD card initialization or card-level query failed.
    Card(SdCardError),
    /// The provided path is empty, malformed, or uses unsupported components.
    InvalidPath(&'static str),
    /// The requested operation is not supported by the underlying filesystem
    /// crate.
    Unsupported(&'static str),
}

pub type Result<T> = core::result::Result<T, Error>;

pub struct SdCard<'a, 'd> {
    card_size_bytes: u64,
    volume_mgr: VolumeManagerType<'a, 'd>,
}

impl<'a, 'd> SdCard<'a, 'd> {
    /// Create a new SD card interface using the default fixed timestamp source.
    ///
    /// The card's chip-select is owned (and parked) by the [`Bus`], so this
    /// takes no pins; the bus must outlive the card.
    pub fn new(bus: &'a Bus<'d>) -> Result<Self> {
        Self::new_with_time_source(bus, SdTimeSource::default())
    }

    /// Create a new SD card interface using a caller-provided timestamp source.
    pub fn new_with_time_source(bus: &'a Bus<'d>, time_source: SdTimeSource) -> Result<Self> {
        // acquisition must run at <=400 kHz (SD spec); the clock is raised below
        // once the card is up.
        let acquire = SpiConfig::default()
            .with_frequency(Rate::from_khz(400))
            .with_mode(SpiMode::_0);

        // sd wake: >=74 clocks with chip-select de-asserted. done on the bus
        // directly (a SharedSpiDevice transaction would assert cs); both
        // chip-selects are parked high by the bus.
        {
            let mut spi = bus.spi.borrow_mut();
            spi.apply_config(&acquire).map_err(Error::SpiConfig)?;
            spi.write(&[0xFF; 10]).map_err(Error::Spi)?;
        }

        let device = SharedSpiDevice::new(&bus.spi, &bus.sd_cs, acquire);
        let sd_card = embedded_sdmmc::SdCard::new(device, Delay::new());
        // num_bytes() forces acquisition; after it, raise the clock so transfers
        // aren't glacial — a 1.5 MB read at 400 kHz takes ~30s versus well under
        // a second at 20 MHz.
        let card_size_bytes = sd_card.num_bytes().map_err(Error::Card)?;
        sd_card.spi(|device| {
            device.set_config(
                SpiConfig::default()
                    .with_frequency(Rate::from_mhz(20))
                    .with_mode(SpiMode::_0),
            )
        });
        let volume_mgr = VolumeManager::new(sd_card, time_source);

        Ok(Self {
            card_size_bytes,
            volume_mgr,
        })
    }

    pub fn card_size_bytes(&self) -> u64 {
        self.card_size_bytes
    }

    /// Return whether a file or directory exists at `path`.
    ///
    /// Paths are slash-delimited, may start with `/`, and must not contain `.`
    /// or `..` path segments.
    pub fn exists(&self, path: &str) -> Result<bool> {
        if is_root(path) {
            return Ok(true);
        }

        match self.metadata(path) {
            Ok(_) => Ok(true),
            Err(Error::Filesystem(embedded_sdmmc::Error::NotFound)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Return metadata for a file or directory at `path`.
    ///
    /// The returned entry contains the full requested path, display name,
    /// directory flag, and size in bytes.
    pub fn metadata(&self, path: &str) -> Result<Metadata> {
        if is_root(path) {
            return Ok(DirectoryEntry {
                path: "/".into(),
                name: "/".into(),
                is_directory: true,
                size: 0,
            });
        }

        let components = path_components(path)?;
        let (parent_components, name) = split_parent_name(&components)?;
        let volume = self.open_volume0()?;
        let mut dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        change_dir_all(&mut dir, &parent_components).map_err(Error::Filesystem)?;
        let entry = dir.find_directory_entry(name).map_err(Error::Filesystem)?;
        Ok(build_directory_entry_for_path(&entry, None, path))
    }

    /// Read an entire file into memory.
    ///
    /// Parent directories must already exist.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let components = path_components(path)?;
        let (parent_components, name) = split_parent_name(&components)?;
        let volume = self.open_volume0()?;
        let mut dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        change_dir_all(&mut dir, &parent_components).map_err(Error::Filesystem)?;
        // open by the on-disk short name resolved from the directory listing.
        // passing the long name straight to `open_file_in_dir` would try to
        // parse it as an 8.3 name and fail for long names or >3-char extensions
        // (e.g. `.epub`).
        let short = resolve_short_name(&dir, name).map_err(Error::Filesystem)?;
        let file = dir
            .open_file_in_dir(short, Mode::ReadOnly)
            .map_err(Error::Filesystem)?;
        let mut data = Vec::new();

        while !file.is_eof() {
            let mut buffer = [0u8; 64];
            let count = file.read(&mut buffer).map_err(Error::Filesystem)?;
            data.extend_from_slice(&buffer[..count]);
        }

        Ok(data)
    }

    /// Create or truncate a file and write `contents`.
    ///
    /// Parent directories must already exist.
    pub fn write_file(&self, path: &str, contents: &[u8]) -> Result<()> {
        self.write_file_with_mode(path, contents, Mode::ReadWriteCreateOrTruncate)
    }

    /// Append `contents` to a file, creating it if it does not already exist.
    ///
    /// Parent directories must already exist.
    pub fn append_file(&self, path: &str, contents: &[u8]) -> Result<()> {
        self.write_file_with_mode(path, contents, Mode::ReadWriteCreateOrAppend)
    }

    /// Delete a file at `path`.
    ///
    /// This only removes files. Removing directories is not supported here.
    pub fn delete_file(&self, path: &str) -> Result<()> {
        let components = path_components(path)?;
        let (parent_components, name) = split_parent_name(&components)?;
        let volume = self.open_volume0()?;
        let mut dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        change_dir_all(&mut dir, &parent_components).map_err(Error::Filesystem)?;
        dir.delete_file_in_dir(name).map_err(Error::Filesystem)
    }

    /// Create a single directory at `path`.
    ///
    /// The parent directory must already exist.
    pub fn create_dir(&self, path: &str) -> Result<()> {
        let components = path_components(path)?;
        let (parent_components, name) = split_parent_name(&components)?;
        let volume = self.open_volume0()?;
        let mut dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        change_dir_all(&mut dir, &parent_components).map_err(Error::Filesystem)?;
        dir.make_dir_in_dir(name).map_err(Error::Filesystem)
    }

    /// Create a directory tree, creating any missing path segments.
    ///
    /// If an existing path component is a file rather than a directory, this
    /// returns [`Error::InvalidPath`].
    pub fn create_dir_all(&self, path: &str) -> Result<()> {
        let components = path_components(path)?;
        let volume = self.open_volume0()?;
        let mut dir = volume.open_root_dir().map_err(Error::Filesystem)?;

        for segment in components {
            match dir.find_directory_entry(segment) {
                Ok(entry) => {
                    if !entry.attributes.is_directory() {
                        return Err(Error::InvalidPath("path component is not a directory"));
                    }
                }
                Err(embedded_sdmmc::Error::NotFound) => {
                    dir.make_dir_in_dir(segment).map_err(Error::Filesystem)?;
                }
                Err(err) => return Err(Error::Filesystem(err)),
            }
            dir.change_dir(segment).map_err(Error::Filesystem)?;
        }

        Ok(())
    }

    /// List the contents of a directory.
    ///
    /// Returned entries include full child paths relative to the queried
    /// directory.
    pub fn list_dir(&self, path: &str) -> Result<Vec<DirectoryEntry>> {
        let components = path_components(path)?;
        let volume = self.open_volume0()?;
        let mut dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        change_dir_all(&mut dir, &components).map_err(Error::Filesystem)?;

        let mut lfn_storage = [0u8; 260];
        let mut lfn_buffer = LfnBuffer::new(&mut lfn_storage);
        let mut entries = Vec::new();

        dir.iterate_dir_lfn(&mut lfn_buffer, |entry, long_name| {
            entries.push(build_directory_entry_for_path(
                entry,
                long_name,
                &join_path(path, &display_name(entry, long_name)),
            ));
        })
        .map_err(Error::Filesystem)?;

        Ok(entries)
    }

    /// Rename a file from `from` to `to`.
    ///
    /// This is implemented as read, write, and delete because
    /// `embedded_sdmmc` does not currently expose a native rename API.
    /// Parent directories for `to` must already exist.
    pub fn rename_file(&self, from: &str, to: &str) -> Result<()> {
        let from_meta = self.metadata(from)?;
        if from_meta.is_directory {
            return Err(Error::Unsupported(
                "directory rename is not supported by embedded-sdmmc",
            ));
        }
        if self.exists(to)? {
            return Err(Error::InvalidPath("destination already exists"));
        }

        let contents = self.read_file(from)?;
        self.write_file(to, &contents)?;
        self.delete_file(from)
    }

    /// Remove a directory.
    ///
    /// This always returns [`Error::Unsupported`] because the current
    /// `embedded_sdmmc` API does not expose directory removal.
    pub fn remove_dir(&self, _path: &str) -> Result<()> {
        Err(Error::Unsupported(
            "directory removal is not supported by embedded-sdmmc",
        ))
    }

    fn open_volume0(
        &self,
    ) -> Result<embedded_sdmmc::Volume<'_, BlockDevice<'a, 'd>, SdTimeSource, 4, 4, 1>> {
        self.volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(Error::Filesystem)
    }

    fn write_file_with_mode(&self, path: &str, contents: &[u8], mode: Mode) -> Result<()> {
        let components = path_components(path)?;
        let (parent_components, name) = split_parent_name(&components)?;
        let volume = self.open_volume0()?;
        let mut dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        change_dir_all(&mut dir, &parent_components).map_err(Error::Filesystem)?;
        let file = dir
            .open_file_in_dir(name, mode)
            .map_err(Error::Filesystem)?;
        file.write(contents).map_err(Error::Filesystem)?;
        file.flush().map_err(Error::Filesystem)
    }
}

fn build_directory_entry_for_path(
    entry: &DirEntry,
    long_name: Option<&str>,
    path: &str,
) -> DirectoryEntry {
    DirectoryEntry {
        path: path.into(),
        name: display_name(entry, long_name),
        is_directory: entry.attributes.is_directory(),
        size: entry.size,
    }
}

fn display_name(entry: &DirEntry, long_name: Option<&str>) -> String {
    long_name
        .map(String::from)
        .unwrap_or_else(|| format!("{}", entry.name))
}

fn change_dir_all<
    'a,
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    dir: &mut embedded_sdmmc::Directory<'a, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    components: &[&str],
) -> core::result::Result<(), embedded_sdmmc::Error<D::Error>>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    for segment in components {
        dir.change_dir(*segment)?;
    }
    Ok(())
}

// resolve a (possibly long) file name within a directory to its on-disk 8.3
// short name by scanning the directory's entries. matches the long name when
// present, otherwise the short name, both case-insensitively.
fn resolve_short_name<
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    dir: &embedded_sdmmc::Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    name: &str,
) -> core::result::Result<ShortFileName, embedded_sdmmc::Error<D::Error>>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let mut lfn_storage = [0u8; 260];
    let mut lfn_buffer = LfnBuffer::new(&mut lfn_storage);
    let mut found: Option<ShortFileName> = None;
    dir.iterate_dir_lfn(&mut lfn_buffer, |entry, long_name| {
        if found.is_none()
            && (long_name.is_some_and(|long| long.eq_ignore_ascii_case(name))
                || format!("{}", entry.name).eq_ignore_ascii_case(name))
        {
            found = Some(entry.name.clone());
        }
    })?;
    found.ok_or(embedded_sdmmc::Error::NotFound)
}

fn split_parent_name<'a>(components: &'a [&'a str]) -> Result<(Vec<&'a str>, &'a str)> {
    if components.is_empty() {
        return Err(Error::InvalidPath(
            "path must include a file or directory name",
        ));
    }
    let (name, parent) = components.split_last().ok_or(Error::InvalidPath(
        "path must include a file or directory name",
    ))?;
    Ok((parent.to_vec(), *name))
}

fn path_components(path: &str) -> Result<Vec<&str>> {
    if path.is_empty() {
        return Err(Error::InvalidPath("path is empty"));
    }

    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut components = Vec::new();
    for component in trimmed.split('/') {
        if component.is_empty() {
            return Err(Error::InvalidPath("path contains an empty component"));
        }
        if matches!(component, "." | "..") {
            return Err(Error::InvalidPath(
                "relative path components are not supported",
            ));
        }
        components.push(component);
    }
    Ok(components)
}

fn is_root(path: &str) -> bool {
    path.trim_matches('/').is_empty()
}

fn join_path(base: &str, name: &str) -> String {
    if is_root(base) {
        format!("/{}", name)
    } else {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }
}
