//! Conflicts a merge, rebase or cherry-pick left in files, and resolving them inside the app.
//!
//! A text conflict is read from the index's three versions of the file (the common ancestor, what HEAD has, and
//! what is being applied) with `git merge-file --diff3`, so every block knows what the ancestor had there whatever
//! conflict style the user's git writes. The file on disk is read too: when it is still git's own conflict, nothing
//! is lost by working from the index; when it was edited by hand, its own markers are what is shown.
//!
//! Nothing here writes a file the user did not ask to write, and a file that changed on disk since it was read is
//! not overwritten. A file is marked resolved (`git add`) only once no block in it is left undecided.
//!
//! git calls the two sides "ours" and "theirs" and swaps their meaning in a rebase. Here they are [`Side::Current`]
//! (what HEAD already has) and [`Side::Incoming`] (what is being applied), and the app names them by meaning.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::backend::{Error, GitCli};

/// One version of a conflicted file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// What HEAD already has: git's "ours", index stage 2. The branch merged into, the new base of a rebase, the
    /// branch a commit is picked onto.
    Current,
    /// What is being applied: git's "theirs", index stage 3. The merged branch, the commit a rebase is replaying,
    /// the picked commit.
    Incoming,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Current => Side::Incoming,
            Side::Incoming => Side::Current,
        }
    }
}

/// A stretch of a conflicted file. Lines keep their line endings, so putting them back together gives the bytes
/// that were read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Lines with no conflict: both sides agree, or only one side changed them.
    Clean(Vec<String>),
    Block(Block),
}

/// Lines both sides changed differently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Block {
    pub current: Vec<String>,
    /// What the common ancestor had here; `None` when that is not known (the file has no common ancestor, or it was
    /// edited by hand and its markers have no base section).
    pub base: Option<Vec<String>>,
    pub incoming: Vec<String>,
}

impl Block {
    pub fn side(&self, side: Side) -> &[String] {
        match side {
            Side::Current => &self.current,
            Side::Incoming => &self.incoming,
        }
    }
}

/// What to put in place of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Current,
    Incoming,
    /// Both, what HEAD has first.
    CurrentThenIncoming,
    /// Both, what is being applied first.
    IncomingThenCurrent,
    /// Neither: the lines as the common ancestor had them.
    Base,
}

impl Resolution {
    /// The lines this choice puts in place of `block`, as two runs one after the other; `None` for [`Resolution::Base`]
    /// when the base is not known.
    pub fn parts(self, block: &Block) -> Option<[&[String]; 2]> {
        let none: &[String] = &[];
        Some(match self {
            Resolution::Current => [&block.current, none],
            Resolution::Incoming => [&block.incoming, none],
            Resolution::CurrentThenIncoming => [&block.current, &block.incoming],
            Resolution::IncomingThenCurrent => [&block.incoming, &block.current],
            Resolution::Base => [block.base.as_deref()?, none],
        })
    }

    /// How many lines this choice gives `block`.
    pub fn len(self, block: &Block) -> usize {
        self.parts(block).map_or(0, |[a, b]| a.len() + b.len())
    }

    /// Line `index` of what this choice gives `block`, and the side it came from (`None` for the base).
    pub fn line(self, block: &Block, index: usize) -> Option<(&str, Option<Side>)> {
        let [first, second] = self.parts(block)?;
        let (first_side, second_side) = match self {
            Resolution::Current => (Some(Side::Current), None),
            Resolution::Incoming => (Some(Side::Incoming), None),
            Resolution::CurrentThenIncoming => (Some(Side::Current), Some(Side::Incoming)),
            Resolution::IncomingThenCurrent => (Some(Side::Incoming), Some(Side::Current)),
            Resolution::Base => (None, None),
        };
        match first.get(index) {
            Some(line) => Some((line.as_str(), first_side)),
            None => second.get(index - first.len()).map(|line| (line.as_str(), second_side)),
        }
    }
}

/// The words written on conflict markers for blocks left undecided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Labels {
    pub current: String,
    pub base: String,
    pub incoming: String,
}

/// The text of a file put back together, and the blocks still undecided in it (written with conflict markers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assembled {
    pub text: String,
    pub unresolved: Vec<usize>,
}

/// Puts a file back together from its segments and a choice for each block. A block with no choice is written with
/// conflict markers `marker_size` long (with its base section when the base is known), so the file can be finished in
/// an editor.
pub fn assemble(segments: &[Segment], choices: &[Option<Resolution>], labels: &Labels, marker_size: usize) -> Assembled {
    let mut text = String::new();
    let mut unresolved = Vec::new();
    let mut block = 0;
    for segment in segments {
        match segment {
            Segment::Clean(lines) => push_lines(&mut text, lines),
            Segment::Block(conflict) => {
                match choices.get(block).copied().flatten().and_then(|choice| choice.parts(conflict)) {
                    Some([first, second]) => {
                        push_lines(&mut text, first);
                        push_lines(&mut text, second);
                    }
                    None => {
                        unresolved.push(block);
                        let eol = line_ending(segments);
                        let marker = |text: &mut String, c: char, label: &str| {
                            ensure_line_end(text, eol);
                            text.extend(std::iter::repeat_n(c, marker_size));
                            if !label.is_empty() {
                                text.push(' ');
                                text.push_str(&one_line(label));
                            }
                            text.push_str(eol);
                        };
                        marker(&mut text, '<', &labels.current);
                        push_lines(&mut text, &conflict.current);
                        if let Some(base) = &conflict.base {
                            marker(&mut text, '|', &labels.base);
                            push_lines(&mut text, base);
                        }
                        marker(&mut text, '=', "");
                        push_lines(&mut text, &conflict.incoming);
                        marker(&mut text, '>', &labels.incoming);
                    }
                }
                block += 1;
            }
        }
    }
    Assembled { text, unresolved }
}

/// Adds `lines` to `text`; a line before them that has no line ending (the end of one side) gets one first, so two
/// runs never run together on one line.
fn push_lines(text: &mut String, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let eol = if lines[0].ends_with("\r\n") { "\r\n" } else { "\n" };
    ensure_line_end(text, eol);
    for line in lines {
        text.push_str(line);
    }
}

fn ensure_line_end(text: &mut String, eol: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str(eol);
    }
}

/// The line ending the file mostly uses, for markers git did not write.
fn line_ending(segments: &[Segment]) -> &'static str {
    let mut crlf = 0usize;
    let mut lf = 0usize;
    let mut count = |lines: &[String]| {
        for line in lines {
            if line.ends_with("\r\n") {
                crlf += 1;
            } else if line.ends_with('\n') {
                lf += 1;
            }
        }
    };
    for segment in segments {
        match segment {
            Segment::Clean(lines) => count(lines),
            Segment::Block(block) => {
                count(&block.current);
                count(&block.incoming);
            }
        }
    }
    if crlf > lf { "\r\n" } else { "\n" }
}

/// A label on one line: a marker line cannot hold a line break.
fn one_line(label: &str) -> String {
    label.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// How many blocks a file's segments hold.
pub fn block_count(segments: &[Segment]) -> usize {
    segments.iter().filter(|s| matches!(s, Segment::Block(_))).count()
}

/// The blocks of a file's segments, in order.
pub fn blocks(segments: &[Segment]) -> impl Iterator<Item = &Block> {
    segments.iter().filter_map(|s| match s {
        Segment::Block(block) => Some(block),
        Segment::Clean(_) => None,
    })
}

// ---- reading conflict markers ---------------------------------------------------------------------

/// Markers that do not make whole conflicts: a block that is not closed, or closed without its middle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrokenMarkers {
    /// The line (from 1) where reading went wrong.
    pub line: usize,
}

/// What a marker line of `size` characters is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Start,
    Base,
    Middle,
    End,
}

fn marker_of(line: &str, size: usize) -> Option<Marker> {
    let text = line.trim_end_matches(['\n', '\r']);
    let first = text.chars().next()?;
    let kind = match first {
        '<' => Marker::Start,
        '|' => Marker::Base,
        '=' => Marker::Middle,
        '>' => Marker::End,
        _ => return None,
    };
    let run = text.chars().take_while(|&c| c == first).count();
    if run != size {
        return None;
    }
    let rest = &text[size..];
    match kind {
        // The middle carries no label.
        Marker::Middle => rest.trim().is_empty().then_some(kind),
        _ => (rest.is_empty() || rest.starts_with(' ')).then_some(kind),
    }
}

/// Splits text into lines that keep their line endings; the last may have none.
pub fn split_lines(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_owned).collect()
}

/// Reads conflict markers `size` characters long (`<<<<<<<`, an optional `|||||||` base, `=======`, `>>>>>>>`).
/// Marker-like lines outside a block, or of another length (a nested conflict, a Markdown rule), are text.
pub fn parse_markers(text: &str, size: usize) -> Result<Vec<Segment>, BrokenMarkers> {
    #[derive(PartialEq)]
    enum In {
        Clean,
        Current,
        Base,
        Incoming,
    }
    let mut segments = Vec::new();
    let mut clean: Vec<String> = Vec::new();
    let mut block = Block::default();
    let mut state = In::Clean;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let broken = || BrokenMarkers { line: n + 1 };
        match (marker_of(line, size), &state) {
            (Some(Marker::Start), In::Clean) => {
                if !clean.is_empty() {
                    segments.push(Segment::Clean(std::mem::take(&mut clean)));
                }
                state = In::Current;
            }
            (Some(Marker::Start), _) => return Err(broken()),
            (Some(Marker::Base), In::Current) => {
                block.base = Some(Vec::new());
                state = In::Base;
            }
            (Some(Marker::Middle), In::Current | In::Base) => state = In::Incoming,
            (Some(Marker::End), In::Incoming) => {
                segments.push(Segment::Block(std::mem::take(&mut block)));
                state = In::Clean;
            }
            (Some(Marker::Base | Marker::Middle | Marker::End), In::Current | In::Base | In::Incoming) => return Err(broken()),
            (_, In::Clean) => clean.push(line.to_owned()),
            (_, In::Current) => block.current.push(line.to_owned()),
            (_, In::Base) => block.base.get_or_insert_with(Vec::new).push(line.to_owned()),
            (_, In::Incoming) => block.incoming.push(line.to_owned()),
        }
    }
    if state != In::Clean {
        return Err(BrokenMarkers { line: text.split_inclusive('\n').count() });
    }
    if !clean.is_empty() {
        segments.push(Segment::Clean(clean));
    }
    Ok(segments)
}

/// A marker size longer than any marker-like run at the start of a line in `texts`, so markers git writes with it
/// can never be confused with the file's own text.
fn safe_marker_size(texts: &[&str]) -> usize {
    let longest = texts
        .iter()
        .flat_map(|text| text.lines())
        .filter_map(|line| {
            let first = line.chars().next().filter(|c| matches!(c, '<' | '|' | '=' | '>'))?;
            Some(line.chars().take_while(|&c| c == first).count())
        })
        .max()
        .unwrap_or(0);
    (longest + 1).max(7)
}

/// The same conflict, however it is split into blocks and whatever the labels on its markers: taking every block from
/// one side gives the same file as the other's, for both sides. Git's own merge and `merge-file` group nearby
/// conflicts differently, and a file git ended with a newline it did not have may differ in that last newline.
fn same_sides(a: &[Segment], b: &[Segment]) -> bool {
    let none = Labels { current: String::new(), base: String::new(), incoming: String::new() };
    [Resolution::Current, Resolution::Incoming].into_iter().all(|side| {
        let whole = |segments: &[Segment]| {
            let text = assemble(segments, &vec![Some(side); block_count(segments)], &none, 7).text;
            text.trim_end_matches(['\n', '\r']).to_owned()
        };
        whole(a) == whole(b)
    })
}

/// A side of the last block that git ended with a line break the side's own file does not have (the end of a file
/// with no final newline) loses it again.
fn trim_added_newline(segments: &mut [Segment], ends: [bool; 3]) {
    let Some(Segment::Block(block)) = segments.last_mut() else { return };
    let trim = |lines: &mut Vec<String>, ends_with_newline: bool| {
        if !ends_with_newline && let Some(last) = lines.last_mut() {
            let kept = last.trim_end_matches(['\n', '\r']).len();
            last.truncate(kept);
        }
    };
    trim(&mut block.current, ends[0]);
    if let Some(base) = block.base.as_mut() {
        trim(base, ends[1]);
    }
    trim(&mut block.incoming, ends[2]);
}

// ---- safe resolutions -----------------------------------------------------------------------------

/// Why a block can be resolved without asking, or what is suggested for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Both sides made the same change; they differ only in spacing, indentation or line endings.
    SameChange,
    /// This side only changed spacing, indentation or line endings here; the other side's change is taken.
    OnlySpacing(Side),
    /// This side left these lines as they were; the other side's change is taken.
    Untouched(Side),
    /// Both sides added lines at the same place and the ancestor had none there. Keeping both is only a suggestion:
    /// the order, or whether both belong, is for a person to say.
    BothAdded,
}

/// What [`classify`] found for a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub resolution: Resolution,
    pub reason: Reason,
    /// Safe to apply without asking. A suggestion is shown and waits for a yes.
    pub certain: bool,
}

/// Files where indentation changes what the code means: there only trailing spaces and line endings count as
/// spacing.
pub fn indentation_matters(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if matches!(name.as_str(), "makefile" | "gnumakefile") {
        return true;
    }
    let ext = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    matches!(
        ext,
        "py" | "pyi" | "pyw" | "yaml" | "yml" | "haml" | "pug" | "jade" | "sass" | "styl" | "coffee" | "nim" | "elm" | "hs" | "lhs"
            | "fs" | "fsx" | "fsi" | "mk" | "gd"
    )
}

/// A line with the spacing that does not matter taken off: its line ending and trailing spaces always, its
/// indentation too unless `indentation_matters`. Spaces inside a line are left alone: they can be inside a string.
fn trimmed(line: &str, indentation_matters: bool) -> &str {
    let line = line.trim_end();
    if indentation_matters { line } else { line.trim_start() }
}

fn same_ignoring_spacing(a: &[String], b: &[String], indentation_matters: bool) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| trimmed(x, indentation_matters) == trimmed(y, indentation_matters))
}

/// Sorts a block by what can be done with it without guessing:
/// - both sides the same apart from spacing: certain, keep what HEAD has;
/// - one side the same as the ancestor apart from spacing: certain, take the other side's real change;
/// - both sides added different lines where the ancestor had none: a suggestion to keep both.
///
/// Anything else is for a person to decide; no merge of meaning is ever guessed.
pub fn classify(block: &Block, indentation_matters: bool) -> Option<Verdict> {
    let certain = |resolution, reason| Some(Verdict { resolution, reason, certain: true });
    if same_ignoring_spacing(&block.current, &block.incoming, indentation_matters) {
        return certain(Resolution::Current, Reason::SameChange);
    }
    let base = block.base.as_ref()?;
    for side in [Side::Current, Side::Incoming] {
        if same_ignoring_spacing(block.side(side), base, indentation_matters) {
            let take = if side == Side::Current { Resolution::Incoming } else { Resolution::Current };
            let reason = if block.side(side) == base.as_slice() { Reason::Untouched(side) } else { Reason::OnlySpacing(side) };
            return certain(take, reason);
        }
    }
    let blank = |lines: &[String]| lines.iter().all(|l| l.trim().is_empty());
    if blank(base) && !blank(&block.current) && !blank(&block.incoming) {
        return Some(Verdict { resolution: Resolution::CurrentThenIncoming, reason: Reason::BothAdded, certain: false });
    }
    None
}

// ---- the index ------------------------------------------------------------------------------------

/// One version of a file in the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    /// `100644`, `100755`, `120000` (a symbolic link) or `160000` (a submodule).
    pub mode: String,
    pub id: String,
}

/// A path git could not merge, and the versions the index holds of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unmerged {
    pub path: String,
    /// The common ancestor's version; none when both sides added the file.
    pub base: Option<Stage>,
    pub current: Option<Stage>,
    pub incoming: Option<Stage>,
}

impl Unmerged {
    pub fn side(&self, side: Side) -> Option<&Stage> {
        match side {
            Side::Current => self.current.as_ref(),
            Side::Incoming => self.incoming.as_ref(),
        }
    }
}

/// Reads `git ls-files -u -z`: `mode id stage\tpath\0`, up to three entries per path, in path order.
pub(crate) fn parse_unmerged(out: &[u8]) -> Vec<Unmerged> {
    let mut found: Vec<Unmerged> = Vec::new();
    for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let text = String::from_utf8_lossy(entry);
        let Some((info, path)) = text.split_once('\t') else { continue };
        let mut fields = info.split(' ');
        let (Some(mode), Some(id), Some(stage)) = (fields.next(), fields.next(), fields.next()) else { continue };
        let stage_of = Stage { mode: mode.to_owned(), id: id.to_owned() };
        if found.last().is_none_or(|last| last.path != path) {
            found.push(Unmerged { path: path.to_owned(), ..Default::default() });
        }
        let last = found.last_mut().expect("just pushed");
        match stage {
            "1" => last.base = Some(stage_of),
            "2" => last.current = Some(stage_of),
            "3" => last.incoming = Some(stage_of),
            _ => {}
        }
    }
    found
}

// ---- one conflicted file --------------------------------------------------------------------------

/// What kind of conflict a file has, and so what can be done about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Text both sides changed: blocks to choose in. No blocks means the file on disk has no markers left.
    Text(TextConflict),
    /// The file's markers do not make whole conflicts (an edit left half done): it is fixed in an editor.
    Broken { line: usize },
    /// One side deleted the file and the other changed it: keep the changed file or delete it.
    Deleted { by: Side },
    /// Both sides deleted the file (or each renamed it): it can only be marked deleted.
    BothDeleted,
    /// Only one side has the file, though git still counts it as a conflict (a rename it could not pair up).
    OnlyOn(Side),
    /// Not text (a binary file or a symbolic link): one whole version is chosen.
    Whole,
    /// A submodule: its commit is chosen in a terminal.
    Submodule,
}

/// A text conflict, ready to choose in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextConflict {
    pub segments: Vec<Segment>,
    /// The blocks come from the index, so each knows its base, and the file on disk is git's own conflict, untouched:
    /// writing over it loses nothing. Otherwise the file was edited by hand and its own markers are shown.
    pub pristine: bool,
    /// How long the markers are in this file (`conflict-marker-size`, 7 by default).
    pub marker_size: usize,
}

/// A conflicted file: its path, the versions the index holds, the file on disk as read, and its kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub stages: Unmerged,
    /// The bytes on disk when read (none when there is no file), so a write can tell that nothing changed them since.
    pub disk: Option<Vec<u8>>,
    pub kind: ConflictKind,
}

/// Text git would merge line by line: UTF-8 with no NUL in its first 8000 bytes (git's own test for binary).
fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// A folder for the three versions `git merge-file` reads, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("gitgui-merge-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn file(&self, name: &str, text: &str) -> std::io::Result<PathBuf> {
        let path = self.0.join(name);
        std::fs::write(&path, text)?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl GitCli {
    /// The paths git could not merge, with the versions the index holds of each.
    pub fn unmerged(&self) -> Result<Vec<Unmerged>, Error> {
        Ok(parse_unmerged(&self.run(&["ls-files", "-u", "-z"])?))
    }

    fn blob(&self, id: &str) -> Result<Vec<u8>, Error> {
        self.run(&["cat-file", "blob", id])
    }

    /// How long conflict markers are in `path`: its `conflict-marker-size` attribute, else 7.
    fn marker_size_of(&self, path: &str) -> usize {
        self.run(&["check-attr", "conflict-marker-size", "--", path])
            .ok()
            .and_then(|out| String::from_utf8_lossy(&out).trim().rsplit(": ").next().and_then(|n| n.parse().ok()))
            .filter(|&n| n > 0)
            .unwrap_or(7)
    }

    /// Reads one conflicted file.
    pub fn conflict(&self, path: &str) -> Result<Conflict, Error> {
        let stages = self
            .unmerged()?
            .into_iter()
            .find(|u| u.path == path)
            .ok_or_else(|| Error::Parse(format!("{path} has no conflict left: it may have been resolved already.")))?;
        let disk = std::fs::read(self.root().join(path)).ok();
        let kind = self.conflict_kind(&stages, disk.as_deref())?;
        Ok(Conflict { path: path.to_owned(), stages, disk, kind })
    }

    fn conflict_kind(&self, stages: &Unmerged, disk: Option<&[u8]>) -> Result<ConflictKind, Error> {
        let (current, incoming) = match (&stages.current, &stages.incoming) {
            (Some(current), Some(incoming)) => (current, incoming),
            (Some(_), None) if stages.base.is_some() => return Ok(ConflictKind::Deleted { by: Side::Incoming }),
            (None, Some(_)) if stages.base.is_some() => return Ok(ConflictKind::Deleted { by: Side::Current }),
            (Some(_), None) => return Ok(ConflictKind::OnlyOn(Side::Current)),
            (None, Some(_)) => return Ok(ConflictKind::OnlyOn(Side::Incoming)),
            (None, None) => return Ok(ConflictKind::BothDeleted),
        };
        let modes = [Some(current), Some(incoming), stages.base.as_ref()];
        if modes.iter().flatten().any(|s| s.mode == "160000") {
            return Ok(ConflictKind::Submodule);
        }
        if modes.iter().flatten().any(|s| s.mode == "120000") {
            return Ok(ConflictKind::Whole);
        }
        let current_bytes = self.blob(&current.id)?;
        let incoming_bytes = self.blob(&incoming.id)?;
        let base_bytes = match &stages.base {
            Some(base) => Some(self.blob(&base.id)?),
            None => None,
        };
        let (Some(current_text), Some(incoming_text)) = (as_text(&current_bytes), as_text(&incoming_bytes)) else {
            return Ok(ConflictKind::Whole);
        };
        let base_text = match base_bytes.as_deref().map(as_text) {
            Some(None) => return Ok(ConflictKind::Whole),
            Some(Some(text)) => Some(text),
            None => None,
        };
        let marker_size = self.marker_size_of(&stages.path);
        let from_index = self.merge_texts(current_text, base_text, incoming_text);
        let Some(disk_text) = disk.and_then(as_text) else {
            // The file is gone from disk or is not text any more: work from the index.
            return Ok(match from_index {
                Some(segments) => ConflictKind::Text(TextConflict { segments, pristine: false, marker_size }),
                None => ConflictKind::Whole,
            });
        };
        let on_disk = match parse_markers(disk_text, marker_size) {
            Ok(segments) => segments,
            Err(broken) => return Ok(ConflictKind::Broken { line: broken.line }),
        };
        if block_count(&on_disk) == 0 {
            // Resolved by hand: nothing to choose, only to mark resolved.
            return Ok(ConflictKind::Text(TextConflict { segments: on_disk, pristine: false, marker_size }));
        }
        // Git's own conflict, untouched? Then the index's blocks, each with its base, are the same conflict.
        Ok(ConflictKind::Text(match from_index.filter(|index| same_sides(&on_disk, index)) {
            Some(segments) => TextConflict { segments, pristine: true, marker_size },
            None => TextConflict { segments: on_disk, pristine: false, marker_size },
        }))
    }

    /// Merges three versions with `git merge-file` and reads the result, each block with its base (`--diff3`). With no
    /// common ancestor, both sides are merged as additions to an empty file and no block has a base. `None` when git
    /// cannot merge them (a binary file).
    fn merge_texts(&self, current: &str, base: Option<&str>, incoming: &str) -> Option<Vec<Segment>> {
        let size = safe_marker_size(&[current, base.unwrap_or(""), incoming]);
        let scratch = Scratch::new().ok()?;
        let files = [scratch.file("current", current).ok()?, scratch.file("base", base.unwrap_or("")).ok()?, scratch.file("incoming", incoming).ok()?];
        let marker = format!("--marker-size={size}");
        let run = |extra: &[&str]| {
            let mut args: Vec<&str> = vec!["merge-file", "-p", &marker, "-L", "current", "-L", "base", "-L", "incoming"];
            if base.is_some() {
                args.push("--diff3");
            }
            args.extend(extra);
            let files: Vec<&str> = files.iter().map(|f| f.to_str().unwrap_or_default()).collect();
            args.extend(files);
            self.command(&args).output().ok()
        };
        // The diff algorithm git's own merges use; an older git that does not know the option merges as it can.
        let output = run(&["--diff-algorithm=histogram"]).filter(|o| o.status.code() != Some(129)).or_else(|| run(&[]))?;
        // The exit code is the number of conflicts (up to 127); anything else is an error, such as a binary file.
        if !matches!(output.status.code(), Some(0..=127)) {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let mut segments = parse_markers(&text, size).ok()?;
        trim_added_newline(&mut segments, [current.ends_with('\n'), base.is_none_or(|b| b.ends_with('\n')), incoming.ends_with('\n')]);
        Some(segments)
    }

    /// Fails unless `path` is exactly as it was read (`expected`, or absent for `None`).
    fn check_unchanged(&self, path: &str, expected: Option<&[u8]>) -> Result<(), Error> {
        let now = std::fs::read(self.root().join(path)).ok();
        if now.as_deref() != expected {
            return Err(Error::Parse(format!("{path} changed on disk since it was read. Re-read it, then choose again.")));
        }
        Ok(())
    }

    /// Writes the resolved text of a conflicted file, and marks it resolved (`git add`) when `resolved` says no block is
    /// left undecided in it. Refuses when the file changed on disk since it was read (`expected`).
    pub fn write_resolution(&self, path: &str, text: &str, expected: Option<&[u8]>, resolved: bool) -> Result<(), Error> {
        check_path(path)?;
        self.check_unchanged(path, expected)?;
        std::fs::write(self.root().join(path), text).map_err(|err| Error::Parse(format!("could not write {path}: {err}")))?;
        if resolved {
            self.mark_resolved(path)?;
        }
        Ok(())
    }

    /// Marks a conflicted file resolved as it is on disk. Refuses a file that still has conflict markers.
    pub fn mark_resolved(&self, path: &str) -> Result<(), Error> {
        check_path(path)?;
        if let Ok(bytes) = std::fs::read(self.root().join(path))
            && let Some(text) = as_text(&bytes)
        {
            match parse_markers(text, self.marker_size_of(path)) {
                Ok(segments) if block_count(&segments) == 0 => {}
                _ => return Err(Error::Parse(format!("{path} still has conflict markers; resolve them first."))),
            }
        }
        self.write(&["add", "--", path]).map(|_| ())
    }

    /// Resolves a file by taking one side's whole version: for a binary file, or a file one side deleted and the other
    /// changed (keeping the changed file). Refuses when the file changed on disk since it was read.
    pub fn take_side(&self, path: &str, side: Side, expected: Option<&[u8]>) -> Result<(), Error> {
        check_path(path)?;
        self.check_unchanged(path, expected)?;
        let which = match side {
            Side::Current => "--ours",
            Side::Incoming => "--theirs",
        };
        self.write(&["checkout", which, "--", path])?;
        self.write(&["add", "--", path]).map(|_| ())
    }

    /// Resolves a conflicted file by deleting it, from the index and from disk.
    pub fn delete_conflicted(&self, path: &str, expected: Option<&[u8]>) -> Result<(), Error> {
        check_path(path)?;
        self.check_unchanged(path, expected)?;
        self.write(&["rm", "-q", "--", path]).map(|_| ())
    }

    /// Puts a text conflict back as the index has it, with each block's base and the markers labelled by meaning,
    /// over whatever is on disk now: the file's own "start over".
    pub fn restart_conflict(&self, path: &str, labels: &Labels) -> Result<(), Error> {
        check_path(path)?;
        let stages = self
            .unmerged()?
            .into_iter()
            .find(|u| u.path == path)
            .ok_or_else(|| Error::Parse(format!("{path} has no conflict left to start over.")))?;
        let (Some(current), Some(incoming)) = (&stages.current, &stages.incoming) else {
            return Err(Error::Parse(format!("{path} is not a text conflict.")));
        };
        let texts = (self.blob(&current.id)?, self.blob(&incoming.id)?, stages.base.as_ref().map(|b| self.blob(&b.id)).transpose()?);
        let (Some(current), Some(incoming)) = (as_text(&texts.0), as_text(&texts.1)) else {
            return Err(Error::Parse(format!("{path} is not text.")));
        };
        let base = texts.2.as_deref().and_then(as_text);
        let segments = self
            .merge_texts(current, base, incoming)
            .ok_or_else(|| Error::Parse(format!("git could not merge {path} again.")))?;
        let text = assemble(&segments, &[], labels, self.marker_size_of(path)).text;
        std::fs::write(self.root().join(path), text).map_err(|err| Error::Parse(format!("could not write {path}: {err}")))
    }
}

/// A path from git's own list, checked before it is handed back to git: never an option.
fn check_path(path: &str) -> Result<(), Error> {
    if path.is_empty() || path.starts_with('-') || Path::new(path).is_absolute() || path.split('/').any(|part| part == "..") {
        return Err(Error::Parse(format!("not a usable path: {path:?}")));
    }
    Ok(())
}

// ---- who changed each side --------------------------------------------------------------------------

/// A commit that changed the conflicted file on one side since the two sides split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideCommit {
    pub id: String,
    pub author: String,
    pub email: String,
    /// Seconds since the epoch.
    pub time: i64,
    pub summary: String,
}

/// The commits that changed a conflicted file on each side since the sides split, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Histories {
    pub current: Vec<SideCommit>,
    pub incoming: Vec<SideCommit>,
    /// Commits left out of each list because it was long.
    pub more_current: usize,
    pub more_incoming: usize,
}

/// How many commits each side lists.
const HISTORY_SHOWN: usize = 12;

/// Reads `git log --format=%H%x1f%aN%x1f%aE%x1f%at%x1f%s%x1e`.
pub(crate) fn parse_side_commits(out: &str) -> Vec<SideCommit> {
    out.split('\x1e')
        .map(|record| record.trim_start_matches('\n'))
        .filter(|record| !record.trim().is_empty())
        .filter_map(|record| {
            let mut fields = record.splitn(5, '\x1f');
            Some(SideCommit {
                id: fields.next()?.to_owned(),
                author: fields.next()?.to_owned(),
                email: fields.next()?.to_owned(),
                time: fields.next()?.parse().ok()?,
                summary: fields.next()?.trim_end().to_owned(),
            })
        })
        .collect()
}

impl GitCli {
    /// The commits on each side that changed `path` since the sides split: from their merge base to HEAD, and to the
    /// commit being applied (`incoming`, a full id).
    pub fn conflict_histories(&self, path: &str, incoming: &str) -> Result<Histories, Error> {
        check_path(path)?;
        if incoming.is_empty() || incoming.starts_with('-') {
            return Err(Error::Parse("no commit is being applied".into()));
        }
        let base = String::from_utf8_lossy(&self.run(&["merge-base", "HEAD", incoming])?).trim().to_owned();
        let side = |tip: &str| -> Result<(Vec<SideCommit>, usize), Error> {
            let range = if base.is_empty() { tip.to_owned() } else { format!("{base}..{tip}") };
            let limit = format!("--max-count={}", HISTORY_SHOWN + 1);
            let out = self.run(&["log", "--format=%H%x1f%aN%x1f%aE%x1f%at%x1f%s%x1e", &limit, &range, "--", path])?;
            let mut commits = parse_side_commits(&String::from_utf8_lossy(&out));
            if commits.len() <= HISTORY_SHOWN {
                return Ok((commits, 0));
            }
            commits.truncate(HISTORY_SHOWN);
            let count = self.run(&["rev-list", "--count", &range, "--", path])?;
            let total: usize = String::from_utf8_lossy(&count).trim().parse().unwrap_or(HISTORY_SHOWN);
            Ok((commits, total.saturating_sub(HISTORY_SHOWN)))
        };
        let (current, more_current) = side("HEAD")?;
        let (incoming, more_incoming) = side(incoming)?;
        Ok(Histories { current, incoming, more_current, more_incoming })
    }
}

// ---- who last changed each block --------------------------------------------------------------------

/// The commit that last changed some of a block's lines on one side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastChange {
    pub commit: String,
    pub author: String,
    pub time: i64,
    pub summary: String,
}

/// For each block, the newest commit among the ones that last changed its lines, on each side.
pub type BlockChanges = Vec<[Option<LastChange>; 2]>;

/// Line `n` (from 1) of `a`, as numbered in `b`, through a `-U0` diff of `a` against `b`: lines outside its hunks are
/// the same in both; lines inside them have no match.
pub(crate) fn map_line(hunks: &[(u32, u32, u32, u32)], n: u32) -> Option<u32> {
    let mut shift: i64 = 0;
    for &(old_start, old_len, _new_start, new_len) in hunks {
        // A hunk that removes nothing sits after line `old_start`.
        let first = if old_len == 0 { old_start + 1 } else { old_start };
        if n < first {
            break;
        }
        if old_len > 0 && n < old_start + old_len {
            return None;
        }
        shift += i64::from(new_len) - i64::from(old_len);
    }
    u32::try_from(i64::from(n) + shift).ok()
}

/// The hunk ranges of a `-U0` diff: `(old start, old length, new start, new length)` from each `@@` line.
pub(crate) fn hunk_ranges(diff: &str) -> Vec<(u32, u32, u32, u32)> {
    let range = |text: &str| -> Option<(u32, u32)> {
        let text = &text[1..];
        Some(match text.split_once(',') {
            Some((start, len)) => (start.parse().ok()?, len.parse().ok()?),
            None => (text.parse().ok()?, 1),
        })
    };
    diff.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("@@ ")?;
            let mut parts = rest.split(' ');
            let (old, new) = (range(parts.next()?)?, range(parts.next()?)?);
            Some((old.0, old.1, new.0, new.1))
        })
        .collect()
}

impl GitCli {
    /// Who last changed each block's lines on each side: `git blame` of the side's commit, through a map from the lines
    /// of the merged file (every block taken from that side) to the lines of the side's own file. Only for a conflict
    /// read from the index; a side whose commit has another version of the file (a rename) is left out.
    pub fn block_changes(&self, conflict: &Conflict, incoming: &str) -> Result<BlockChanges, Error> {
        let ConflictKind::Text(text) = &conflict.kind else { return Ok(Vec::new()) };
        if !text.pristine {
            return Ok(Vec::new());
        }
        let mut changes: BlockChanges = vec![[None, None]; block_count(&text.segments)];
        for (slot, side, commit) in [(0, Side::Current, "HEAD"), (1, Side::Incoming, incoming)] {
            let Some(stage) = conflict.stages.side(side) else { continue };
            // The side's commit must hold exactly this version of the file, for its blame to number the same lines.
            let spec = format!("{commit}:{}", conflict.path);
            let at = self.run(&["rev-parse", "--verify", "--quiet", &spec]).map(|out| String::from_utf8_lossy(&out).trim().to_owned());
            if at.ok().as_deref() != Some(stage.id.as_str()) {
                continue;
            }
            let all = vec![Some(if side == Side::Current { Resolution::Current } else { Resolution::Incoming }); changes.len()];
            let merged = assemble(&text.segments, &all, &Labels { current: String::new(), base: String::new(), incoming: String::new() }, 7).text;
            let scratch = Scratch::new().map_err(|err| Error::Parse(err.to_string()))?;
            let merged_file = scratch.file("merged", &merged).map_err(|err| Error::Parse(err.to_string()))?;
            let side_text = String::from_utf8_lossy(&self.blob(&stage.id)?).into_owned();
            let side_file = scratch.file("side", &side_text).map_err(|err| Error::Parse(err.to_string()))?;
            let output = self
                .command(&["diff", "--no-index", "--no-color", "--no-ext-diff", "-U0", "--", path_str(&merged_file), path_str(&side_file)])
                .output()
                .map_err(Error::Spawn)?;
            if !matches!(output.status.code(), Some(0 | 1)) {
                continue;
            }
            let hunks = hunk_ranges(&String::from_utf8_lossy(&output.stdout));
            let blame = self.blame_with(Some(commit), &conflict.path, false)?;
            // Walk the merged file: each block's lines are where its side's lines landed.
            let mut line = 1u32;
            let mut block = 0;
            for segment in &text.segments {
                match segment {
                    Segment::Clean(lines) => line += lines.len() as u32,
                    Segment::Block(b) => {
                        let count = b.side(side).len() as u32;
                        let newest = (line..line + count)
                            .filter_map(|n| blame.line(map_line(&hunks, n)?))
                            .filter(|info| !info.uncommitted())
                            .max_by_key(|info| info.time);
                        changes[block][slot] = newest.map(|info| LastChange {
                            commit: info.commit.clone(),
                            author: info.author.clone(),
                            time: info.time,
                            summary: info.summary.clone(),
                        });
                        line += count;
                        block += 1;
                    }
                }
            }
        }
        Ok(changes)
    }
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        split_lines(text)
    }

    fn labels() -> Labels {
        Labels { current: "On main".into(), base: "base".into(), incoming: "Coming in from feature".into() }
    }

    const DIFF3: &str = "top\n<<<<<<< ours\nmine\n||||||| base\nold\n=======\nyours\n>>>>>>> theirs\nmiddle\n<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\n";

    #[test]
    fn markers_are_read_into_clean_text_and_blocks_with_and_without_a_base() {
        let segments = parse_markers(DIFF3, 7).unwrap();
        assert_eq!(
            segments,
            [
                Segment::Clean(lines("top\n")),
                Segment::Block(Block { current: lines("mine\n"), base: Some(lines("old\n")), incoming: lines("yours\n") }),
                Segment::Clean(lines("middle\n")),
                Segment::Block(Block { current: lines("a\n"), base: None, incoming: lines("b\n") }),
            ]
        );
        assert_eq!(block_count(&segments), 2);
    }

    #[test]
    fn choosing_every_block_gives_back_a_file_with_no_markers() {
        let segments = parse_markers(DIFF3, 7).unwrap();
        let done = assemble(&segments, &[Some(Resolution::Incoming), Some(Resolution::CurrentThenIncoming)], &labels(), 7);
        assert_eq!(done.text, "top\nyours\nmiddle\na\nb\n");
        assert!(done.unresolved.is_empty());
        let base = assemble(&segments, &[Some(Resolution::Base), Some(Resolution::IncomingThenCurrent)], &labels(), 7);
        assert_eq!(base.text, "top\nold\nmiddle\nb\na\n");
    }

    #[test]
    fn an_undecided_block_is_written_back_with_markers_named_by_meaning() {
        let segments = parse_markers(DIFF3, 7).unwrap();
        let half = assemble(&segments, &[None, Some(Resolution::Current)], &labels(), 7);
        assert_eq!(half.unresolved, [0]);
        assert_eq!(half.text, "top\n<<<<<<< On main\nmine\n||||||| base\nold\n=======\nyours\n>>>>>>> Coming in from feature\nmiddle\na\n");
        // Read back, it is the same conflict, with its base.
        let again = parse_markers(&half.text, 7).unwrap();
        assert_eq!(block_count(&again), 1);
        assert!(blocks(&again).next().unwrap().base.is_some());
    }

    #[test]
    fn the_base_of_a_block_that_has_none_is_not_a_choice() {
        let block = Block { current: lines("a\n"), base: None, incoming: lines("b\n") };
        assert_eq!(Resolution::Base.parts(&block), None);
        let segments = vec![Segment::Block(block)];
        assert_eq!(assemble(&segments, &[Some(Resolution::Base)], &labels(), 7).unresolved, [0]);
    }

    #[test]
    fn a_marker_must_be_exactly_its_size_and_stray_ones_outside_a_block_are_text() {
        // A Markdown underline and a longer run are text; a lone middle or end marker outside a block too.
        let text = "Title\n=======\n<<<<<<<<< nine\n>>>>>>> stray\n";
        assert_eq!(parse_markers(text, 7).unwrap(), [Segment::Clean(lines(text))]);
        // A nested conflict with longer markers stays inside its outer block's side.
        let nested = "<<<<<<< a\n<<<<<<<<< inner\nx\n=========\ny\n>>>>>>>>> inner\n=======\nz\n>>>>>>> b\n";
        let segments = parse_markers(nested, 7).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(blocks(&segments).next().unwrap().current.len(), 5);
    }

    #[test]
    fn unfinished_markers_are_reported_with_their_line() {
        assert_eq!(parse_markers("a\n<<<<<<< x\nb\n", 7), Err(BrokenMarkers { line: 3 }));
        assert_eq!(parse_markers("<<<<<<< x\nb\n>>>>>>> y\n", 7), Err(BrokenMarkers { line: 3 }), "an end with no middle");
        assert_eq!(parse_markers("<<<<<<< x\n<<<<<<< y\n", 7), Err(BrokenMarkers { line: 2 }));
    }

    #[test]
    fn crlf_files_keep_their_line_endings_through_reading_and_writing() {
        let text = "a\r\n<<<<<<< x\r\nB1\r\n||||||| b\r\nb\r\n=======\r\nB2\r\n>>>>>>> y\r\nz\r\n";
        let segments = parse_markers(text, 7).unwrap();
        assert_eq!(blocks(&segments).next().unwrap().current, ["B1\r\n"]);
        assert_eq!(assemble(&segments, &[Some(Resolution::Incoming)], &labels(), 7).text, "a\r\nB2\r\nz\r\n");
        // Markers the app writes follow the file's line endings.
        let half = assemble(&segments, &[None], &labels(), 7).text;
        assert!(half.contains("<<<<<<< On main\r\n") && half.contains("=======\r\n"), "{half:?}");
    }

    #[test]
    fn a_last_line_with_no_newline_is_not_run_into_the_next_side() {
        let block = Block { current: vec!["end C".into()], base: Some(vec!["end".into()]), incoming: vec!["end I".into()] };
        let segments = vec![Segment::Clean(lines("x\n")), Segment::Block(block)];
        assert_eq!(assemble(&segments, &[Some(Resolution::Current)], &labels(), 7).text, "x\nend C");
        assert_eq!(assemble(&segments, &[Some(Resolution::CurrentThenIncoming)], &labels(), 7).text, "x\nend C\nend I");
        assert_eq!(assemble(&segments, &[None], &labels(), 7).text, "x\n<<<<<<< On main\nend C\n||||||| base\nend\n=======\nend I\n>>>>>>> Coming in from feature\n");
    }

    #[test]
    fn git_s_extra_newline_after_a_side_that_had_none_is_taken_off_again() {
        let mut segments = parse_markers("x\n<<<<<<< c\nlastC\n||||||| b\nlast\n=======\nlastI\n>>>>>>> i\n", 7).unwrap();
        trim_added_newline(&mut segments, [false, false, true]);
        let block = blocks(&segments).next().unwrap();
        assert_eq!((block.current[0].as_str(), block.base.as_ref().unwrap()[0].as_str(), block.incoming[0].as_str()), ("lastC", "last", "lastI\n"));
    }

    #[test]
    fn empty_sides_and_adjacent_blocks_are_kept_apart() {
        let text = "<<<<<<< a\n=======\nadded\n>>>>>>> b\n<<<<<<< a\nx\n=======\n>>>>>>> b\n";
        let segments = parse_markers(text, 7).unwrap();
        assert_eq!(segments.len(), 2, "two blocks with nothing between them");
        let choices = [Some(Resolution::Current), Some(Resolution::Incoming)];
        assert_eq!(assemble(&segments, &choices, &labels(), 7).text, "", "an empty side is an empty result");
        let other = [Some(Resolution::Incoming), Some(Resolution::Current)];
        assert_eq!(assemble(&segments, &other, &labels(), 7).text, "added\nx\n");
    }

    #[test]
    fn a_very_long_file_reads_in_one_pass() {
        let mut text = String::new();
        for i in 0..50_000 {
            text.push_str(&format!("line {i}\n"));
            if i % 5_000 == 0 {
                text.push_str("<<<<<<< a\nmine\n=======\nyours\n>>>>>>> b\n");
            }
        }
        let started = std::time::Instant::now();
        let segments = parse_markers(&text, 7).unwrap();
        assert_eq!(block_count(&segments), 10);
        let all = vec![Some(Resolution::Current); 10];
        assert_eq!(assemble(&segments, &all, &labels(), 7).text.lines().count(), 50_010);
        assert!(started.elapsed().as_secs_f64() < 2., "took {:?}", started.elapsed());
    }

    #[test]
    fn the_marker_size_for_git_is_longer_than_anything_in_the_files() {
        assert_eq!(safe_marker_size(&["plain\n", ""]), 7);
        assert_eq!(safe_marker_size(&["<<<<<<< a\n", "x\n========== rule\n"]), 11);
    }

    #[test]
    fn the_same_conflict_is_recognised_whatever_its_labels_and_grouping() {
        let git = parse_markers("<<<<<<< HEAD\na\n=======\nb\n>>>>>>> 34996b0 (feat 1)\n}\n<<<<<<< HEAD\nc\n=======\nd\n>>>>>>> 34996b0 (feat 1)\nz", 7).unwrap();
        let ours = parse_markers("<<<<<<<<< current\na\n}\nc\n||||||||| base\nq\n}\nq\n=========\nb\n}\nd\n>>>>>>>>> incoming\nz\n", 9).unwrap();
        assert!(same_sides(&git, &ours), "two blocks or one, with or without a base, and a last newline git added");
        let edited = parse_markers("<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x\n}\nc\nz", 7).unwrap();
        assert!(!same_sides(&edited, &ours), "a block resolved by hand is a different file");
    }

    fn block(current: &str, base: Option<&str>, incoming: &str) -> Block {
        Block { current: lines(current), base: base.map(lines), incoming: lines(incoming) }
    }

    #[test]
    fn the_same_change_spaced_differently_is_certain() {
        let verdict = classify(&block("  let x = 1;\n", Some("let x = 0;\n"), "let x = 1;   \r\n"), false).unwrap();
        assert_eq!((verdict.resolution, verdict.reason, verdict.certain), (Resolution::Current, Reason::SameChange, true));
    }

    #[test]
    fn a_side_that_only_reindented_gives_way_to_the_real_change() {
        let verdict = classify(&block("    old();\n", Some("old();\n"), "new();\n"), false).unwrap();
        assert_eq!((verdict.resolution, verdict.reason, verdict.certain), (Resolution::Incoming, Reason::OnlySpacing(Side::Current), true));
        let verdict = classify(&block("new();\n", Some("old();\n"), "old();\n"), false).unwrap();
        assert_eq!((verdict.resolution, verdict.reason), (Resolution::Current, Reason::Untouched(Side::Incoming)));
    }

    #[test]
    fn where_indentation_means_something_reindenting_is_a_real_change() {
        assert!(indentation_matters("app/models.py") && indentation_matters("ci/.gitlab-ci.yml") && indentation_matters("Makefile"));
        assert!(!indentation_matters("src/main.rs"));
        let reindented = block("    old()\n", Some("old()\n"), "new()\n");
        assert_eq!(classify(&reindented, true), None, "Python: the indentation is the code");
        let trailing = block("old()   \n", Some("old()\n"), "new()\n");
        assert_eq!(classify(&trailing, true).map(|v| v.reason), Some(Reason::OnlySpacing(Side::Current)), "trailing spaces never count");
    }

    #[test]
    fn spaces_inside_a_line_count_as_a_change() {
        // They may be inside a string.
        assert_eq!(classify(&block("say(\"a  b\");\n", Some("say(\"a b\");\n"), "shout(\"a b\");\n"), false), None);
    }

    #[test]
    fn lines_added_by_both_where_the_base_had_none_suggest_keeping_both() {
        let verdict = classify(&block("fn a() {}\n", Some(""), "fn b() {}\n"), false).unwrap();
        assert_eq!((verdict.resolution, verdict.reason, verdict.certain), (Resolution::CurrentThenIncoming, Reason::BothAdded, false));
        assert_eq!(classify(&block("fn a() {}\n", None, "fn b() {}\n"), false), None, "with no base, nothing is suggested");
        assert_eq!(classify(&block("x = 2\n", Some("x = 1\n"), "x = 3\n"), false), None, "a real conflict is left to a person");
    }

    #[test]
    fn a_resolution_reads_line_by_line_with_where_each_line_came_from() {
        let b = block("a\nb\n", Some("o\n"), "c\n");
        assert_eq!(Resolution::IncomingThenCurrent.len(&b), 3);
        assert_eq!(Resolution::IncomingThenCurrent.line(&b, 0), Some(("c\n", Some(Side::Incoming))));
        assert_eq!(Resolution::IncomingThenCurrent.line(&b, 2), Some(("b\n", Some(Side::Current))));
        assert_eq!(Resolution::IncomingThenCurrent.line(&b, 3), None);
        assert_eq!(Resolution::Base.line(&b, 0), Some(("o\n", None)));
    }

    #[test]
    fn the_index_lists_each_conflicted_path_with_the_versions_it_has() {
        let out = b"100644 aaa 1\tboth.txt\x00100644 bbb 2\tboth.txt\x00100644 ccc 3\tboth.txt\x00100644 ddd 2\tnew.txt\x00\
                    100644 eee 3\tnew.txt\x00100644 fff 1\tgone.txt\x00100644 ggg 3\tgone.txt\x00";
        let found = parse_unmerged(out);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].path, "both.txt");
        assert_eq!(found[0].base.as_ref().map(|s| s.id.as_str()), Some("aaa"));
        assert_eq!(found[1].base, None, "added on both sides");
        assert_eq!((found[2].current.is_none(), found[2].incoming.as_ref().map(|s| s.id.as_str())), (true, Some("ggg")), "deleted by HEAD");
        assert_eq!(found[2].side(Side::Incoming).map(|s| s.mode.as_str()), Some("100644"));
    }

    #[test]
    fn side_commits_are_read_from_the_log() {
        let out = "abc\x1fAda\x1fada@x.dev\x1f1700000000\x1fRaise the timeout\x1e\ndef\x1fBo\x1fbo@x.dev\x1f1700000500\x1fMake: retries | configurable\x1e\n";
        let commits = parse_side_commits(out);
        assert_eq!(commits.len(), 2);
        assert_eq!((commits[0].id.as_str(), commits[0].author.as_str(), commits[0].time), ("abc", "Ada", 1_700_000_000));
        assert_eq!(commits[1].summary, "Make: retries | configurable");
        assert!(parse_side_commits("").is_empty());
    }

    #[test]
    fn lines_map_through_a_diff_and_changed_lines_do_not() {
        // a: 1 2 3 4 5 6   b: 1 X 3 4 Y Y 6 (line 2 changed, a line added after 4, line 5 changed)
        let hunks = hunk_ranges("@@ -2 +2 @@\n-2\n+X\n@@ -5 +5,2 @@\n-5\n+Y\n+Y\n");
        assert_eq!(hunks, [(2, 1, 2, 1), (5, 1, 5, 2)]);
        assert_eq!(map_line(&hunks, 1), Some(1));
        assert_eq!(map_line(&hunks, 2), None);
        assert_eq!(map_line(&hunks, 4), Some(4));
        assert_eq!(map_line(&hunks, 5), None);
        assert_eq!(map_line(&hunks, 6), Some(7));
        // A pure insertion after line 1 shifts what follows.
        let inserted = hunk_ranges("@@ -1,0 +2,3 @@\n+a\n+b\n+c\n");
        assert_eq!(map_line(&inserted, 1), Some(1));
        assert_eq!(map_line(&inserted, 2), Some(5));
    }

    #[test]
    fn paths_that_could_be_options_never_reach_git() {
        for bad in ["", "-f", "--force", "/etc/passwd", "a/../../b"] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
        assert!(check_path("src/a b.rs").is_ok());
    }
}
