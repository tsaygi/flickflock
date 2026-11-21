//! Folder-to-folder navigation (siblings or whole tree), the end-of-folder
//! policy and the back/forward history stack.

use crate::folder::{dir_has_images, is_hidden_name};
use crate::natural::natural_cmp_os;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What happens when stepping past the last (or before the first) image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EndOfFolder {
    Stop,
    Wrap,
    #[default]
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FolderSort {
    #[default]
    Name,
    Date,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// Result of stepping one image forward/backward inside a folder of `len` images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Stay in this folder and show this index.
    Index(usize),
    /// At the boundary with the `Stop` policy.
    Stay,
    /// Leave the folder in the given direction (`Continue` policy).
    Leave,
}

/// Pure end-of-folder decision.
pub fn step(index: usize, len: usize, dir: Direction, policy: EndOfFolder) -> Step {
    if len == 0 {
        return if policy == EndOfFolder::Continue {
            Step::Leave
        } else {
            Step::Stay
        };
    }
    match dir {
        Direction::Forward if index + 1 < len => Step::Index(index + 1),
        Direction::Backward if index > 0 => Step::Index(index - 1),
        _ => match policy {
            EndOfFolder::Stop => Step::Stay,
            EndOfFolder::Wrap => Step::Index(if dir == Direction::Forward {
                0
            } else {
                len - 1
            }),
            EndOfFolder::Continue => Step::Leave,
        },
    }
}

/// Bundle extensions that look like folders but are opaque packages on macOS.
const PACKAGE_EXTENSIONS: &[&str] = &[
    "app",
    "bundle",
    "framework",
    "plugin",
    "kext",
    "photoslibrary",
    "photolibrary",
    "aplibrary",
    "migratedphotolibrary",
    "imovielibrary",
    "tvlibrary",
    "musiclibrary",
    "fcpbundle",
    "lrdata",
    "lrcat-data",
    "xcodeproj",
    "xcworkspace",
    "playground",
    "pkg",
    "mpkg",
    "rtfd",
    "key",
    "pages",
    "numbers",
    "logicx",
    "band",
    "sparsebundle",
    "download",
];

/// Folders that are never entered: dot-folders, folders with the Windows
/// hidden attribute and macOS packages such as `.photoslibrary`.
pub fn is_skipped_folder(path: &Path) -> bool {
    if is_hidden_name(path) {
        return true;
    }
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        if PACKAGE_EXTENSIONS.contains(&ext.as_str()) {
            return true;
        }
    }
    has_hidden_attribute(path)
}

#[cfg(windows)]
fn has_hidden_attribute(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    std::fs::metadata(path)
        .map(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn has_hidden_attribute(_path: &Path) -> bool {
    false
}

/// Where the image-bearing folders come from. Abstracted so the navigator can
/// be tested without a real directory tree and so results can be cached.
pub trait FolderSource {
    /// Visible sub-folders of `dir`, in navigation order.
    fn subfolders(&self, dir: &Path) -> Vec<PathBuf>;
    /// Does `dir` directly contain supported images?
    fn has_images(&self, dir: &Path) -> bool;
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsSource {
    pub sort: FolderSort,
}

impl FolderSource for FsSource {
    fn subfolders(&self, dir: &Path) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<(PathBuf, Option<std::time::SystemTime>)> = rd
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let md = std::fs::metadata(&path).ok()?;
                (md.is_dir() && !is_skipped_folder(&path)).then(|| (path, md.modified().ok()))
            })
            .collect();
        let by_name = |a: &PathBuf, b: &PathBuf| {
            natural_cmp_os(
                a.file_name().unwrap_or_default(),
                b.file_name().unwrap_or_default(),
            )
        };
        match self.sort {
            FolderSort::Name => dirs.sort_by(|a, b| by_name(&a.0, &b.0)),
            FolderSort::Date => {
                dirs.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| by_name(&a.0, &b.0)))
            }
        }
        dirs.into_iter().map(|(p, _)| p).collect()
    }

    fn has_images(&self, dir: &Path) -> bool {
        dir_has_images(dir)
    }
}

/// Finds the next/previous folder that contains images.
#[derive(Debug, Clone, Default)]
pub struct DirectoryNavigator {
    /// Walk the whole tree under `root` depth-first instead of siblings only.
    pub traverse_tree: bool,
    /// Tree traversal never leaves this folder. `None` = parent of the first opened folder.
    pub root: Option<PathBuf>,
}

/// Upper bound on folders inspected per jump, so a pathological tree (or a
/// slow network share full of empty folders) cannot hang the search forever.
const MAX_FOLDERS_PER_SEARCH: usize = 20_000;

impl DirectoryNavigator {
    pub fn new(traverse_tree: bool, root: Option<PathBuf>) -> Self {
        Self {
            traverse_tree,
            root,
        }
    }

    /// The effective traversal root for a search starting at `current`.
    pub fn effective_root(&self, current: &Path) -> PathBuf {
        match &self.root {
            Some(r) if current.starts_with(r) => r.clone(),
            _ => current.parent().unwrap_or(current).to_path_buf(),
        }
    }

    /// Next (or previous) folder with images relative to `current`, or `None`
    /// at the end of the sibling list / tree.
    pub fn neighbor(
        &self,
        current: &Path,
        dir: Direction,
        src: &dyn FolderSource,
    ) -> Option<PathBuf> {
        if self.traverse_tree {
            self.tree_neighbor(current, dir, src)
        } else {
            sibling_neighbor(current, dir, src)
        }
    }

    fn tree_neighbor(
        &self,
        current: &Path,
        dir: Direction,
        src: &dyn FolderSource,
    ) -> Option<PathBuf> {
        let root = self.effective_root(current);
        let mut cursor = current.to_path_buf();
        for _ in 0..MAX_FOLDERS_PER_SEARCH {
            cursor = match dir {
                Direction::Forward => preorder_next(&cursor, &root, src)?,
                Direction::Backward => preorder_prev(&cursor, &root, src)?,
            };
            if src.has_images(&cursor) {
                return Some(cursor);
            }
        }
        None
    }
}

fn sibling_neighbor(current: &Path, dir: Direction, src: &dyn FolderSource) -> Option<PathBuf> {
    let siblings = src.subfolders(current.parent()?);
    let pos = siblings.iter().position(|p| p == current)?;
    match dir {
        Direction::Forward => siblings[pos + 1..]
            .iter()
            .find(|p| src.has_images(p))
            .cloned(),
        Direction::Backward => siblings[..pos]
            .iter()
            .rev()
            .find(|p| src.has_images(p))
            .cloned(),
    }
}

/// Depth-first pre-order successor of `cur` inside `root`: first child, else
/// next sibling, else the next sibling of the nearest ancestor that has one.
fn preorder_next(cur: &Path, root: &Path, src: &dyn FolderSource) -> Option<PathBuf> {
    if let Some(first) = src.subfolders(cur).into_iter().next() {
        return Some(first);
    }
    let mut node = cur.to_path_buf();
    while node != root {
        let parent = node.parent()?.to_path_buf();
        let siblings = src.subfolders(&parent);
        let pos = siblings.iter().position(|p| *p == node)?;
        if let Some(next) = siblings.get(pos + 1) {
            return Some(next.clone());
        }
        node = parent;
    }
    None
}

/// Depth-first pre-order predecessor: the deepest last descendant of the
/// previous sibling, else the parent.
fn preorder_prev(cur: &Path, root: &Path, src: &dyn FolderSource) -> Option<PathBuf> {
    if cur == root {
        return None;
    }
    let parent = cur.parent()?.to_path_buf();
    let siblings = src.subfolders(&parent);
    let pos = siblings.iter().position(|p| p == cur)?;
    if pos == 0 {
        return Some(parent);
    }
    let mut node = siblings[pos - 1].clone();
    while let Some(last) = src.subfolders(&node).into_iter().next_back() {
        node = last;
    }
    Some(node)
}

/// Browser-like back/forward history of viewed images.
///
/// Sequential flicking does not flood the history: a sequential move replaces
/// the top entry, while a *jump* (thumbnail click, folder change, first/last,
/// opening a file) pushes a new one. So after jumping 5 → 300 and flicking on
/// to 301, Back returns to 5 and Forward to 301.
#[derive(Debug, Clone, Default)]
pub struct HistoryStack {
    entries: Vec<PathBuf>,
    pos: usize,
}

const HISTORY_LIMIT: usize = 500;

impl HistoryStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn current(&self) -> Option<&Path> {
        self.entries.get(self.pos).map(PathBuf::as_path)
    }

    /// Record a non-sequential move to `path`.
    pub fn jump(&mut self, path: &Path) {
        if self.current() == Some(path) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.pos + 1);
        }
        self.entries.push(path.to_path_buf());
        if self.entries.len() > HISTORY_LIMIT {
            let extra = self.entries.len() - HISTORY_LIMIT;
            self.entries.drain(..extra);
        }
        self.pos = self.entries.len() - 1;
    }

    /// Record a sequential move (next/previous image): updates the current entry in place.
    pub fn step(&mut self, path: &Path) {
        match self.entries.get_mut(self.pos) {
            Some(top) => *top = path.to_path_buf(),
            None => self.jump(path),
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.pos > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.pos + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<&Path> {
        if !self.can_go_back() {
            return None;
        }
        self.pos -= 1;
        self.current()
    }

    pub fn forward(&mut self) -> Option<&Path> {
        if !self.can_go_forward() {
            return None;
        }
        self.pos += 1;
        self.current()
    }

    /// Forget a path that no longer exists (trashed or renamed away).
    pub fn remove(&mut self, path: &Path) {
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i] == path {
                self.entries.remove(i);
                if i < self.pos || (i == self.pos && self.pos > 0) {
                    self.pos -= 1;
                }
            } else {
                i += 1;
            }
        }
    }

    /// Follow a rename.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        for e in &mut self.entries {
            if e == from {
                *e = to.to_path_buf();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// In-memory folder tree.
    #[derive(Default)]
    struct Fake {
        children: BTreeMap<PathBuf, Vec<PathBuf>>,
        with_images: BTreeSet<PathBuf>,
    }

    impl Fake {
        /// `spec`: ("path", has_images). Parents are created implicitly; order = insertion order.
        fn new(spec: &[(&str, bool)]) -> Self {
            let mut f = Fake::default();
            for (p, img) in spec {
                let path = PathBuf::from(p);
                if *img {
                    f.with_images.insert(path.clone());
                }
                let mut child = path;
                while let Some(parent) = child.parent().map(Path::to_path_buf) {
                    let kids = f.children.entry(parent.clone()).or_default();
                    if !kids.contains(&child) {
                        kids.push(child.clone());
                    }
                    child = parent;
                }
            }
            f
        }
    }

    impl FolderSource for Fake {
        fn subfolders(&self, dir: &Path) -> Vec<PathBuf> {
            self.children.get(dir).cloned().unwrap_or_default()
        }
        fn has_images(&self, dir: &Path) -> bool {
            self.with_images.contains(dir)
        }
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn step_policies() {
        use Direction::*;
        assert_eq!(step(3, 10, Forward, EndOfFolder::Stop), Step::Index(4));
        assert_eq!(step(3, 10, Backward, EndOfFolder::Stop), Step::Index(2));
        assert_eq!(step(9, 10, Forward, EndOfFolder::Stop), Step::Stay);
        assert_eq!(step(0, 10, Backward, EndOfFolder::Stop), Step::Stay);
        assert_eq!(step(9, 10, Forward, EndOfFolder::Wrap), Step::Index(0));
        assert_eq!(step(0, 10, Backward, EndOfFolder::Wrap), Step::Index(9));
        assert_eq!(step(9, 10, Forward, EndOfFolder::Continue), Step::Leave);
        assert_eq!(step(0, 10, Backward, EndOfFolder::Continue), Step::Leave);
        assert_eq!(step(0, 1, Forward, EndOfFolder::Wrap), Step::Index(0));
        assert_eq!(step(0, 0, Forward, EndOfFolder::Continue), Step::Leave);
        assert_eq!(step(0, 0, Forward, EndOfFolder::Stop), Step::Stay);
    }

    #[test]
    fn siblings_skip_empty_folders_both_directions() {
        let fs = Fake::new(&[
            ("/Trip/Day1", true),
            ("/Trip/Day2", false),
            ("/Trip/Day3", true),
        ]);
        let nav = DirectoryNavigator::default();
        assert_eq!(
            nav.neighbor(&p("/Trip/Day1"), Direction::Forward, &fs),
            Some(p("/Trip/Day3"))
        );
        assert_eq!(
            nav.neighbor(&p("/Trip/Day3"), Direction::Backward, &fs),
            Some(p("/Trip/Day1"))
        );
        assert_eq!(
            nav.neighbor(&p("/Trip/Day3"), Direction::Forward, &fs),
            None
        );
        assert_eq!(
            nav.neighbor(&p("/Trip/Day1"), Direction::Backward, &fs),
            None
        );
        // Starting from the empty folder itself still works.
        assert_eq!(
            nav.neighbor(&p("/Trip/Day2"), Direction::Forward, &fs),
            Some(p("/Trip/Day3"))
        );
    }

    #[test]
    fn siblings_do_not_descend_or_climb() {
        let fs = Fake::new(&[
            ("/A/x", true),
            ("/A/x/deep", true),
            ("/A/y", true),
            ("/B", true),
        ]);
        let nav = DirectoryNavigator::default();
        assert_eq!(
            nav.neighbor(&p("/A/x"), Direction::Forward, &fs),
            Some(p("/A/y"))
        );
        assert_eq!(nav.neighbor(&p("/A/y"), Direction::Forward, &fs), None);
    }

    fn tree() -> Fake {
        Fake::new(&[
            ("/R", true),
            ("/R/a", true),
            ("/R/a/a1", true),
            ("/R/a/a2", false),
            ("/R/a/a2/deep", true),
            ("/R/b", false),
            ("/R/c", true),
            ("/Other", true),
        ])
    }

    #[test]
    fn whole_tree_forward_is_depth_first() {
        let fs = tree();
        let nav = DirectoryNavigator::new(true, Some(p("/R")));
        let mut order = vec![p("/R")];
        while let Some(next) = nav.neighbor(order.last().unwrap(), Direction::Forward, &fs) {
            order.push(next);
        }
        assert_eq!(
            order,
            [
                p("/R"),
                p("/R/a"),
                p("/R/a/a1"),
                p("/R/a/a2/deep"),
                p("/R/c")
            ]
        );
    }

    #[test]
    fn whole_tree_backward_mirrors_forward() {
        let fs = tree();
        let nav = DirectoryNavigator::new(true, Some(p("/R")));
        let mut order = vec![p("/R/c")];
        while let Some(prev) = nav.neighbor(order.last().unwrap(), Direction::Backward, &fs) {
            order.push(prev);
        }
        assert_eq!(
            order,
            [
                p("/R/c"),
                p("/R/a/a2/deep"),
                p("/R/a/a1"),
                p("/R/a"),
                p("/R")
            ]
        );
    }

    #[test]
    fn tree_root_defaults_to_parent_and_is_not_escaped() {
        let fs = tree();
        let nav = DirectoryNavigator::new(true, None);
        assert_eq!(nav.effective_root(&p("/R/a")), p("/R"));
        // From /R/c (last under /R) there is nothing more, /Other is outside the root.
        assert_eq!(nav.neighbor(&p("/R/c"), Direction::Forward, &fs), None);
        // A configured root that does not contain the folder falls back to the parent.
        let nav = DirectoryNavigator::new(true, Some(p("/Elsewhere")));
        assert_eq!(nav.effective_root(&p("/R/a")), p("/R"));
    }

    #[test]
    fn real_filesystem_ordering_and_skipping() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Trip");
        for (d, files) in [
            ("Day1", 2),
            ("Day2", 0),
            ("Day10", 1),
            (".hidden", 1),
            ("Lib.photoslibrary", 1),
        ] {
            let dir = root.join(d);
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..files {
                std::fs::write(dir.join(format!("{i}.png")), b"x").unwrap();
            }
        }
        let fs = FsSource::default();
        let subs: Vec<_> = fs
            .subfolders(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(subs, ["Day1", "Day2", "Day10"]);
        let nav = DirectoryNavigator::default();
        assert_eq!(
            nav.neighbor(&root.join("Day1"), Direction::Forward, &fs),
            Some(root.join("Day10"))
        );
        assert_eq!(
            nav.neighbor(&root.join("Day10"), Direction::Backward, &fs),
            Some(root.join("Day1"))
        );
    }

    #[test]
    fn history_jump_then_back_and_forward() {
        let mut h = HistoryStack::new();
        h.jump(&p("/f/5.jpg"));
        h.jump(&p("/f/300.jpg"));
        assert_eq!(h.back(), Some(p("/f/5.jpg").as_path()));
        assert!(!h.can_go_back());
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some(p("/f/300.jpg").as_path()));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn history_sequential_steps_do_not_grow() {
        let mut h = HistoryStack::new();
        h.jump(&p("/f/5.jpg"));
        h.jump(&p("/f/300.jpg"));
        h.step(&p("/f/301.jpg"));
        h.step(&p("/f/302.jpg"));
        assert_eq!(h.len(), 2);
        assert_eq!(h.back(), Some(p("/f/5.jpg").as_path()));
        assert_eq!(h.forward(), Some(p("/f/302.jpg").as_path()));
    }

    #[test]
    fn history_jump_truncates_forward_entries() {
        let mut h = HistoryStack::new();
        for n in ["a", "b", "c"] {
            h.jump(&p(n));
        }
        h.back();
        h.back();
        h.jump(&p("d"));
        assert_eq!(h.len(), 2);
        assert!(!h.can_go_forward());
        assert_eq!(h.back(), Some(p("a").as_path()));
        // Jumping to the current entry is a no-op.
        h.jump(&p("a"));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn history_is_bounded_and_handles_removal() {
        let mut h = HistoryStack::new();
        for i in 0..(HISTORY_LIMIT + 50) {
            h.jump(&p(&format!("{i}")));
        }
        assert_eq!(h.len(), HISTORY_LIMIT);
        assert_eq!(
            h.current(),
            Some(p(&format!("{}", HISTORY_LIMIT + 49)).as_path())
        );

        let mut h = HistoryStack::new();
        for n in ["a", "b", "c"] {
            h.jump(&p(n));
        }
        h.remove(&p("c"));
        assert_eq!(h.current(), Some(p("b").as_path()));
        h.rename(&p("a"), &p("z"));
        assert_eq!(h.back(), Some(p("z").as_path()));
    }

    #[test]
    fn step_on_empty_history_starts_it() {
        let mut h = HistoryStack::new();
        h.step(&p("x"));
        assert_eq!(h.current(), Some(p("x").as_path()));
    }
}
