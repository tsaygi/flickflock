//! Folder indexing: scan a directory for supported images, sort and group
//! them, and watch the directory for external changes.

use crate::decode;
use crate::natural::{natural_cmp, natural_cmp_os};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Name,
    Modified,
    Created,
    Taken,
    Size,
    Dimensions,
    Type,
}

impl SortKey {
    pub const ALL: [SortKey; 7] = [
        Self::Name,
        Self::Modified,
        Self::Created,
        Self::Taken,
        Self::Size,
        Self::Dimensions,
        Self::Type,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Modified => "Date modified",
            Self::Created => "Date created",
            Self::Taken => "Date taken (EXIF)",
            Self::Size => "File size",
            Self::Dimensions => "Dimensions",
            Self::Type => "File type",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKey {
    Type,
    Day,
    Month,
    Year,
    Folder,
}

impl GroupKey {
    pub const ALL: [GroupKey; 5] = [Self::Type, Self::Day, Self::Month, Self::Year, Self::Folder];

    pub fn label(self) -> &'static str {
        match self {
            Self::Type => "File type",
            Self::Day => "Day",
            Self::Month => "Month",
            Self::Year => "Year",
            Self::Folder => "Folder",
        }
    }
}

/// How a folder is ordered and sectioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SortSpec {
    pub key: SortKey,
    pub descending: bool,
    /// Nested grouping, outermost first (e.g. `[Year, Type]`).
    pub group_by: Vec<GroupKey>,
    /// Include images in sub-folders.
    pub recursive: bool,
}

impl SortSpec {
    fn needs_taken(&self) -> bool {
        self.key == SortKey::Taken
    }
    fn needs_dimensions(&self) -> bool {
        self.key == SortKey::Dimensions
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageEntry {
    pub path: PathBuf,
    pub name: OsString,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    /// Lower-case extension ("jpg"), empty when there is none.
    pub ext: String,
    /// EXIF DateTimeOriginal as "YYYY:MM:DD HH:MM:SS" (only read when sorting by it).
    pub taken: Option<String>,
    /// Pixel dimensions (only read when sorting by them).
    pub dimensions: Option<(u32, u32)>,
}

impl ImageEntry {
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let md = std::fs::metadata(path)?;
        Ok(Self::from_metadata(path.to_path_buf(), &md))
    }

    fn from_metadata(path: PathBuf, md: &std::fs::Metadata) -> Self {
        let name = path.file_name().map(OsString::from).unwrap_or_default();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        Self {
            name,
            size: md.len(),
            modified: md.modified().ok(),
            created: md.created().ok(),
            ext,
            taken: None,
            dimensions: None,
            path,
        }
    }

    pub fn display_name(&self) -> String {
        self.name.to_string_lossy().into_owned()
    }
}

/// A section of the sorted entry list (`start..end` indexes into `entries`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// One label per nesting level, e.g. `["2024", "JPG"]`.
    pub labels: Vec<String>,
    pub start: usize,
    pub end: usize,
}

impl Group {
    pub fn title(&self) -> String {
        self.labels.join(" · ")
    }
}

/// All supported images of one folder, sorted and grouped.
#[derive(Debug, Clone, Default)]
pub struct FolderIndex {
    pub dir: PathBuf,
    pub entries: Vec<ImageEntry>,
    pub groups: Vec<Group>,
    pub spec: SortSpec,
}

impl FolderIndex {
    /// Scan `dir` for supported images. Never fails on unreadable entries;
    /// an unreadable directory yields an error.
    pub fn scan(dir: &Path, spec: &SortSpec) -> std::io::Result<Self> {
        let mut entries = Vec::new();
        collect(dir, spec.recursive, &mut entries)?;
        let mut index = Self {
            dir: dir.to_path_buf(),
            entries,
            groups: Vec::new(),
            spec: spec.clone(),
        };
        index.resort(spec);
        Ok(index)
    }

    /// Build an index from explicit entries (used by tests and for single files).
    pub fn from_entries(dir: &Path, entries: Vec<ImageEntry>, spec: &SortSpec) -> Self {
        let mut index = Self {
            dir: dir.to_path_buf(),
            entries,
            groups: Vec::new(),
            spec: spec.clone(),
        };
        index.resort(spec);
        index
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn position(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    pub fn path(&self, i: usize) -> Option<&Path> {
        self.entries.get(i).map(|e| e.path.as_path())
    }

    /// Make sure `path` is part of the index (a file opened explicitly is
    /// always browsable, even with an unknown extension). Returns its position.
    pub fn ensure_contains(&mut self, path: &Path) -> Option<usize> {
        if let Some(i) = self.position(path) {
            return Some(i);
        }
        let entry = ImageEntry::from_path(path).ok()?;
        self.entries.push(entry);
        let spec = self.spec.clone();
        self.resort(&spec);
        self.position(path)
    }

    /// Re-sort and re-group in place with a new spec.
    pub fn resort(&mut self, spec: &SortSpec) {
        if spec.needs_taken() {
            self.entries
                .par_iter_mut()
                .filter(|e| e.taken.is_none())
                .for_each(|e| {
                    e.taken = crate::meta::date_taken(&e.path);
                });
        }
        if spec.needs_dimensions() {
            self.entries
                .par_iter_mut()
                .filter(|e| e.dimensions.is_none())
                .for_each(|e| {
                    e.dimensions = decode::probe_dimensions(&e.path);
                });
        }
        let dir = self.dir.clone();
        let keyed = |e: &ImageEntry| -> Vec<String> {
            spec.group_by
                .iter()
                .map(|g| group_label(e, *g, &dir))
                .collect()
        };
        // Sort by the group labels first (outermost group first), then by the sort key.
        let mut decorated: Vec<(Vec<String>, ImageEntry)> = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|e| (keyed(&e), e))
            .collect();
        decorated.sort_by(|(ga, a), (gb, b)| {
            for (la, lb) in ga.iter().zip(gb) {
                let ord = natural_cmp(la, lb);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            compare_entries(a, b, spec)
        });
        self.groups.clear();
        if !spec.group_by.is_empty() {
            for (i, (labels, _)) in decorated.iter().enumerate() {
                match self.groups.last_mut() {
                    Some(g) if &g.labels == labels => g.end = i + 1,
                    _ => self.groups.push(Group {
                        labels: labels.clone(),
                        start: i,
                        end: i + 1,
                    }),
                }
            }
        }
        self.entries = decorated.into_iter().map(|(_, e)| e).collect();
        self.spec = spec.clone();
    }

    /// The group containing entry `i`, if grouping is active.
    pub fn group_of(&self, i: usize) -> Option<&Group> {
        self.groups.iter().find(|g| g.start <= i && i < g.end)
    }
}

fn collect(dir: &Path, recursive: bool, out: &mut Vec<ImageEntry>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if is_hidden_name(&path) {
            continue;
        }
        // `metadata()` follows symlinks, unlike `DirEntry::metadata`.
        let Ok(md) = std::fs::metadata(&path) else {
            continue;
        };
        if md.is_dir() {
            if recursive && !crate::nav::is_skipped_folder(&path) {
                let _ = collect(&path, true, out);
            }
        } else if md.is_file() && decode::is_supported_path(&path) {
            out.push(ImageEntry::from_metadata(path, &md));
        }
    }
    Ok(())
}

/// Does `dir` directly contain at least one supported image?
pub fn dir_has_images(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        let p = e.path();
        !is_hidden_name(&p) && decode::is_supported_path(&p) && p.is_file()
    })
}

pub(crate) fn is_hidden_name(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

fn compare_entries(a: &ImageEntry, b: &ImageEntry, spec: &SortSpec) -> Ordering {
    let by_name = || natural_cmp_os(&a.name, &b.name).then_with(|| a.path.cmp(&b.path));
    let primary = match spec.key {
        SortKey::Name => Ordering::Equal,
        SortKey::Modified => a.modified.cmp(&b.modified),
        SortKey::Created => a.created.cmp(&b.created),
        SortKey::Taken => effective_date(a).cmp(&effective_date(b)),
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Dimensions => area(a).cmp(&area(b)),
        SortKey::Type => a.ext.cmp(&b.ext),
    };
    let ord = primary.then_with(by_name);
    if spec.descending {
        ord.reverse()
    } else {
        ord
    }
}

fn area(e: &ImageEntry) -> u64 {
    e.dimensions.map(|(w, h)| w as u64 * h as u64).unwrap_or(0)
}

/// Date used for "taken" sorting and date grouping: EXIF date when known,
/// otherwise the modification time. Formatted "YYYY:MM:DD HH:MM:SS" (UTC for
/// file times) so plain string comparison orders chronologically.
fn effective_date(e: &ImageEntry) -> String {
    match &e.taken {
        Some(t) => t.clone(),
        None => e.modified.map(format_system_time).unwrap_or_default(),
    }
}

fn group_label(e: &ImageEntry, key: GroupKey, root: &Path) -> String {
    match key {
        GroupKey::Type => {
            if e.ext.is_empty() {
                "(no extension)".to_owned()
            } else {
                e.ext.to_uppercase()
            }
        }
        GroupKey::Folder => {
            let parent = e.path.parent().unwrap_or(root);
            match parent.strip_prefix(root) {
                Ok(rel) if rel.as_os_str().is_empty() => root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into()),
                Ok(rel) => rel.to_string_lossy().into_owned(),
                Err(_) => parent.to_string_lossy().into_owned(),
            }
        }
        GroupKey::Day | GroupKey::Month | GroupKey::Year => {
            let d = effective_date(e);
            let (y, m, day) = (d.get(0..4), d.get(5..7), d.get(8..10));
            match (key, y, m, day) {
                (GroupKey::Year, Some(y), _, _) => y.to_owned(),
                (GroupKey::Month, Some(y), Some(m), _) => format!("{y}-{m}"),
                (GroupKey::Day, Some(y), Some(m), Some(d)) => format!("{y}-{m}-{d}"),
                _ => "(no date)".to_owned(),
            }
        }
    }
}

/// Format a timestamp as "YYYY:MM:DD HH:MM:SS" in UTC (EXIF style).
pub fn format_system_time(t: SystemTime) -> String {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}:{m:02}:{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Watches a folder and calls `on_change` (from a background thread) whenever
/// something in it is created, removed, renamed or modified. The owner is
/// expected to debounce and re-scan.
pub struct FolderWatcher {
    _watcher: notify::RecommendedWatcher,
    pub dir: PathBuf,
}

impl FolderWatcher {
    pub fn new(
        dir: &Path,
        recursive: bool,
        on_change: impl Fn() + Send + 'static,
    ) -> notify::Result<Self> {
        use notify::{EventKind, RecursiveMode, Watcher};
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    if matches!(
                        ev.kind,
                        EventKind::Create(_)
                            | EventKind::Remove(_)
                            | EventKind::Modify(_)
                            | EventKind::Any
                    ) {
                        on_change();
                    }
                }
            })?;
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher.watch(dir, mode)?;
        Ok(Self {
            _watcher: watcher,
            dir: dir.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(name: &str, size: u64, modified_secs: u64) -> ImageEntry {
        let path = PathBuf::from("/photos").join(name);
        ImageEntry {
            name: path.file_name().unwrap().to_owned(),
            ext: path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
            path,
            size,
            modified: Some(UNIX_EPOCH + Duration::from_secs(modified_secs)),
            created: None,
            taken: None,
            dimensions: None,
        }
    }

    fn names(ix: &FolderIndex) -> Vec<String> {
        ix.entries.iter().map(|e| e.display_name()).collect()
    }

    const DAY: u64 = 86_400;

    fn sample() -> Vec<ImageEntry> {
        vec![
            entry("img10.jpg", 300, 400 * DAY),
            entry("IMG2.JPG", 100, 20_000 * DAY),
            entry("img1.png", 200, 20_000 * DAY + 60),
            entry("写真 3.png", 50, 401 * DAY),
        ]
    }

    #[test]
    fn natural_name_sort() {
        let ix = FolderIndex::from_entries(Path::new("/photos"), sample(), &SortSpec::default());
        assert_eq!(
            names(&ix),
            ["img1.png", "IMG2.JPG", "img10.jpg", "写真 3.png"]
        );
        assert!(ix.groups.is_empty());
    }

    #[test]
    fn sort_by_size_descending() {
        let spec = SortSpec {
            key: SortKey::Size,
            descending: true,
            ..Default::default()
        };
        let ix = FolderIndex::from_entries(Path::new("/photos"), sample(), &spec);
        assert_eq!(
            names(&ix),
            ["img10.jpg", "img1.png", "IMG2.JPG", "写真 3.png"]
        );
    }

    #[test]
    fn group_by_type() {
        let spec = SortSpec {
            group_by: vec![GroupKey::Type],
            ..Default::default()
        };
        let ix = FolderIndex::from_entries(Path::new("/photos"), sample(), &spec);
        assert_eq!(
            names(&ix),
            ["IMG2.JPG", "img10.jpg", "img1.png", "写真 3.png"]
        );
        assert_eq!(ix.groups.len(), 2);
        assert_eq!(
            ix.groups[0],
            Group {
                labels: vec!["JPG".into()],
                start: 0,
                end: 2
            }
        );
        assert_eq!(ix.groups[1].title(), "PNG");
        assert_eq!(ix.group_of(3).unwrap().title(), "PNG");
    }

    #[test]
    fn nested_grouping_year_then_type() {
        let spec = SortSpec {
            group_by: vec![GroupKey::Year, GroupKey::Type],
            ..Default::default()
        };
        let ix = FolderIndex::from_entries(Path::new("/photos"), sample(), &spec);
        let titles: Vec<String> = ix.groups.iter().map(Group::title).collect();
        assert_eq!(
            titles,
            ["1971 · JPG", "1971 · PNG", "2024 · JPG", "2024 · PNG"]
        );
        assert_eq!(
            names(&ix),
            ["img10.jpg", "写真 3.png", "IMG2.JPG", "img1.png"]
        );
        // Groups tile the whole list.
        assert_eq!(ix.groups.first().unwrap().start, 0);
        assert_eq!(ix.groups.last().unwrap().end, ix.len());
    }

    #[test]
    fn group_by_day_and_month() {
        let spec = SortSpec {
            group_by: vec![GroupKey::Day],
            ..Default::default()
        };
        let ix = FolderIndex::from_entries(Path::new("/photos"), sample(), &spec);
        assert_eq!(ix.groups.len(), 3);
        assert_eq!(ix.groups[0].title(), "1971-02-05");
        let spec = SortSpec {
            group_by: vec![GroupKey::Month],
            ..Default::default()
        };
        let ix = FolderIndex::from_entries(Path::new("/photos"), sample(), &spec);
        assert_eq!(ix.groups.len(), 2);
    }

    #[test]
    fn date_formatting() {
        assert_eq!(format_system_time(UNIX_EPOCH), "1970:01:01 00:00:00");
        assert_eq!(
            format_system_time(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000:02:29 00:00:00"
        );
        assert_eq!(
            format_system_time(UNIX_EPOCH + Duration::from_secs(1_735_689_599)),
            "2024:12:31 23:59:59"
        );
    }

    #[test]
    fn scan_real_folder_filters_and_recurses() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        for n in ["b 2.png", "a10.jpg", "a9.JPG", "notes.txt", ".hidden.png"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/z.gif"), b"x").unwrap();
        std::fs::create_dir(d.join(".secret")).unwrap();
        std::fs::write(d.join(".secret/s.gif"), b"x").unwrap();

        let ix = FolderIndex::scan(d, &SortSpec::default()).unwrap();
        assert_eq!(names(&ix), ["a9.JPG", "a10.jpg", "b 2.png"]);
        assert!(dir_has_images(d));
        assert!(!dir_has_images(&d.join("nope")));

        let spec = SortSpec {
            recursive: true,
            group_by: vec![GroupKey::Folder],
            ..Default::default()
        };
        let ix = FolderIndex::scan(d, &spec).unwrap();
        assert_eq!(ix.len(), 4);
        assert_eq!(ix.groups.len(), 2);
        assert_eq!(ix.groups.last().unwrap().title(), "sub");
    }

    #[test]
    fn ensure_contains_adds_odd_files_and_keeps_order() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::write(d.join("a.png"), b"x").unwrap();
        std::fs::write(d.join("c.png"), b"x").unwrap();
        std::fs::write(d.join("b.weird"), b"x").unwrap();
        let mut ix = FolderIndex::scan(d, &SortSpec::default()).unwrap();
        assert_eq!(ix.len(), 2);
        assert_eq!(ix.ensure_contains(&d.join("b.weird")), Some(1));
        assert_eq!(ix.ensure_contains(&d.join("c.png")), Some(2));
        assert_eq!(ix.ensure_contains(&d.join("missing.png")), None);
    }

    #[test]
    fn resort_keeps_entries() {
        let mut ix =
            FolderIndex::from_entries(Path::new("/photos"), sample(), &SortSpec::default());
        let current = ix.entries[2].path.clone();
        ix.resort(&SortSpec {
            key: SortKey::Modified,
            descending: true,
            ..Default::default()
        });
        assert_eq!(
            names(&ix),
            ["img1.png", "IMG2.JPG", "写真 3.png", "img10.jpg"]
        );
        assert_eq!(ix.position(&current), Some(3));
    }

    #[test]
    fn watcher_reports_new_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _w = FolderWatcher::new(tmp.path(), false, move || {
            let _ = tx.send(());
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(tmp.path().join("new.png"), b"x").unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(10)).is_ok());
    }
}
