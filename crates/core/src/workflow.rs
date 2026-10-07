//! A team's branching habits, read from the branches it already has, and the names those habits lead to.
//!
//! Every company names branches differently (`feature/74-driver-reporting`, `feat/vip-report`, `ABC-123-fix-login`) and
//! starts them from a different trunk. Nobody remembers the rule; the repository shows it. [`detect`] reads it, and
//! [`branch_name`] builds a new branch's name from a type, a ticket and a title the way the team does.

use std::collections::HashMap;

/// What a branch name looks like, apart from its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `feature/add-login`
    TypeSlug,
    /// `feature/74-add-login`, or `feature/ABC-74-add-login`
    TypeTicketSlug,
    /// `74-add-login`
    TicketSlug,
    /// `add-login`
    Slug,
}

impl Shape {
    pub const ALL: [Shape; 4] = [Shape::TypeSlug, Shape::TypeTicketSlug, Shape::TicketSlug, Shape::Slug];

    /// How it is kept in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Shape::TypeSlug => "type-slug",
            Shape::TypeTicketSlug => "type-ticket-slug",
            Shape::TicketSlug => "ticket-slug",
            Shape::Slug => "slug",
        }
    }

    pub fn from_id(id: &str) -> Option<Shape> {
        Shape::ALL.into_iter().find(|shape| shape.id() == id)
    }

    /// The pattern, for people.
    pub fn pattern(self) -> &'static str {
        match self {
            Shape::TypeSlug => "type/title",
            Shape::TypeTicketSlug => "type/ticket-title",
            Shape::TicketSlug => "ticket-title",
            Shape::Slug => "title",
        }
    }

    pub fn uses_type(self) -> bool {
        matches!(self, Shape::TypeSlug | Shape::TypeTicketSlug)
    }

    pub fn uses_ticket(self) -> bool {
        matches!(self, Shape::TypeTicketSlug | Shape::TicketSlug)
    }

    /// A name in this shape, as an example.
    pub fn example(self, kind: &str) -> String {
        branch_name(self, kind, "74", "add login")
    }
}

/// Words for a branch name: lowercase letters and digits joined by single dashes, cut at a word before `MOST` characters.
pub fn slugify(text: &str) -> String {
    const MOST: usize = 40;
    let mut slug = String::new();
    let mut gap = false;
    for c in text.chars() {
        if c.is_alphanumeric() && c.is_ascii() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    if slug.len() > MOST {
        slug.truncate(MOST);
        // Not in the middle of a word, when there is a word boundary to cut at.
        if let Some(dash) = slug.rfind('-').filter(|&at| at >= MOST / 2) {
            slug.truncate(dash);
        }
        slug = slug.trim_end_matches('-').to_owned();
    }
    slug
}

/// A ticket as it goes in a name: `#74` is `74`, `abc 123` is `abc-123`; nothing else odd.
pub fn ticket_text(raw: &str) -> String {
    let raw = raw.trim().trim_start_matches('#');
    let joined: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
    joined.join("-").chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect()
}

/// The name of a new branch of `kind` for `ticket` and `title`, in `shape`; empty while there is nothing to name it by.
pub fn branch_name(shape: Shape, kind: &str, ticket: &str, title: &str) -> String {
    let (slug, ticket) = (slugify(title), ticket_text(ticket));
    let body = match (shape.uses_ticket() && !ticket.is_empty(), slug.is_empty()) {
        (true, false) => format!("{ticket}-{slug}"),
        (true, true) => ticket,
        (false, _) => slug,
    };
    let kind = slugify(kind);
    if body.is_empty() {
        String::new()
    } else if shape.uses_type() && !kind.is_empty() {
        format!("{kind}/{body}")
    } else {
        body
    }
}

/// How the team brings a branch back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStyle {
    /// Nothing has been merged yet, or too little to tell.
    Unknown,
    /// Branches are merged with a merge commit.
    Merge,
    /// Branches are squashed into one commit.
    Squash,
    Mixed,
}

/// What the branches say about how the team works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    /// The words before the first `/` that several branches share, the commonest first, with how many branches.
    pub types: Vec<(String, usize)>,
    pub shape: Shape,
    /// Where branches are usually merged into.
    pub base: Option<String>,
    pub merge_style: MergeStyle,
    /// How many branches were read.
    pub branches: usize,
}

/// Prefixes that are tools, not people: their branches say nothing about the team's habit.
const BOTS: [&str; 6] = ["dependabot", "renovate", "snyk", "greenkeeper", "copilot", "imgbot"];
/// The usual order of types, for those that are equally common.
const USUAL: [&str; 9] = ["feature", "feat", "bugfix", "fix", "hotfix", "chore", "docs", "refactor", "test"];
/// A prefix that names a version (`release/1.0.0`), whose rest is not a ticket and a title.
const VERSIONED: [&str; 2] = ["release", "releases"];

/// How long the ticket that opens `rest` is, if one does: `74` in `74-add-login`, `ABC-74` in `ABC-74-add-login`. A
/// tracker key is in capitals; `add-74-things` is a title that happens to have a number in it.
fn ticket_len(rest: &str) -> Option<usize> {
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let len = if digits > 0 {
        digits
    } else {
        let letters = rest.chars().take_while(char::is_ascii_uppercase).count();
        if !(2..=10).contains(&letters) || !rest[letters..].starts_with('-') {
            return None;
        }
        let number = rest[letters + 1..].chars().take_while(char::is_ascii_digit).count();
        if number == 0 {
            return None;
        }
        letters + 1 + number
    };
    let after = &rest[len..];
    (after.is_empty() || after.starts_with('-')).then_some(len)
}

fn starts_with_ticket(rest: &str) -> bool {
    ticket_len(rest).is_some()
}

/// Reads a team's habits from its branch names (local and remote, each once, without the remote's name), the branches
/// they were merged into, how many merges were plain and how many squashed, and the trunk-like branches there are,
/// best first.
pub fn detect(branches: &[&str], merged_into: &[&str], contained: usize, squashed: usize, trunks: &[&str]) -> Detected {
    // The words before the first slash.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for name in branches {
        if let Some((kind, rest)) = name.split_once('/')
            && !kind.is_empty()
            && !rest.is_empty()
            && !BOTS.contains(&kind)
        {
            *counts.entry(kind).or_default() += 1;
        }
    }
    let total = branches.len().max(1);
    let mut types: Vec<(String, usize)> = counts
        .into_iter()
        // Several branches, and not a stray one among many.
        .filter(|&(_, n)| n >= 2 && n * 100 / total >= 5)
        .map(|(kind, n)| (kind.to_owned(), n))
        .collect();
    // The commonest first; for the same count the usual order (feature before bugfix), then by name.
    let usual = |kind: &str| USUAL.iter().position(|u| *u == kind).unwrap_or(USUAL.len());
    types.sort_by(|a, b| b.1.cmp(&a.1).then(usual(&a.0).cmp(&usual(&b.0))).then(a.0.cmp(&b.0)));

    // The shape comes from the branches of work (not releases): do their names open with a ticket?
    let typed_work = branches.iter().filter_map(|name| name.split_once('/')).filter(|(kind, _)| {
        types.iter().any(|(t, _)| t == kind) && !VERSIONED.contains(kind)
    });
    let (mut typed, mut typed_tickets) = (0, 0);
    for (_, rest) in typed_work {
        typed += 1;
        typed_tickets += usize::from(starts_with_ticket(rest));
    }
    let plain = branches.iter().filter(|name| !name.contains('/'));
    let (mut flat, mut flat_tickets) = (0, 0);
    for name in plain {
        flat += 1;
        flat_tickets += usize::from(starts_with_ticket(name));
    }
    let shape = if typed >= 2 || (typed > 0 && flat < typed) {
        if typed_tickets * 2 >= typed { Shape::TypeTicketSlug } else { Shape::TypeSlug }
    } else if flat >= 2 {
        if flat_tickets * 2 >= flat { Shape::TicketSlug } else { Shape::Slug }
    } else {
        Shape::TypeSlug
    };

    let mut targets: HashMap<&str, usize> = HashMap::new();
    for into in merged_into {
        *targets.entry(into).or_default() += 1;
    }
    let base = targets
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        .map(|(name, _)| name.to_owned())
        .or_else(|| trunks.first().map(|name| (*name).to_owned()));

    let merge_style = match (contained, squashed) {
        (0, 0) => MergeStyle::Unknown,
        (_, 0) => MergeStyle::Merge,
        (0, _) => MergeStyle::Squash,
        (a, b) if a >= b * 4 => MergeStyle::Merge,
        (a, b) if b >= a * 4 => MergeStyle::Squash,
        _ => MergeStyle::Mixed,
    };
    Detected { types, shape, base, merge_style, branches: branches.len() }
}

impl Detected {
    /// What was found, in words.
    pub fn describe(&self) -> String {
        let mut said = Vec::new();
        if self.branches == 0 {
            return "There are no branches to learn from yet.".to_owned();
        }
        let kinds: Vec<&str> = self.types.iter().take(4).map(|(kind, _)| kind.as_str()).collect();
        let example = self.shape.example(kinds.first().copied().unwrap_or("feature"));
        said.push(format!("Branches are named like {example}."));
        if !self.types.is_empty() {
            let list: Vec<String> = self.types.iter().take(5).map(|(kind, n)| format!("{kind} ({n})")).collect();
            said.push(format!("Types in use: {}.", list.join(", ")));
        }
        if let Some(base) = &self.base {
            said.push(format!("They are usually merged into {base}."));
        }
        said.push(
            match self.merge_style {
                MergeStyle::Unknown => "Nothing has been merged yet, so how the team merges is not known.",
                MergeStyle::Merge => "Branches come back with a merge commit.",
                MergeStyle::Squash => "Branches come back squashed into one commit.",
                MergeStyle::Mixed => "Branches come back both ways: some with a merge commit, some squashed.",
            }
            .to_owned(),
        );
        said.join(" ")
    }
}

/// What is wrong with a branch name for a team that uses `shape` and `types`; `None` when it fits. Only habits are
/// checked, not git's own rules (git says those itself).
pub fn name_problem(shape: Shape, types: &[String], name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if shape.uses_type() {
        let list = || types.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
        match name.split_once('/') {
            None if types.is_empty() => return Some(format!("This team names branches like {}.", shape.example("feature"))),
            None => return Some(format!("This team starts a branch name with its type and a slash: {}.", list())),
            Some((kind, _)) if !types.is_empty() && !types.iter().any(|t| t == kind) => {
                return Some(format!("“{kind}” is not one of this team's types: {}.", list()));
            }
            Some(_) => {}
        }
    }
    let words = name.rsplit('/').next().unwrap_or(name);
    // Capitals belong to a tracker key (`ABC-9`), not to the words after it.
    let title = ticket_len(words).map_or(words, |len| &words[len..]);
    if title.chars().any(|c| c.is_uppercase()) {
        return Some("This team writes branch names in lowercase, with dashes between words.".to_owned());
    }
    if shape.uses_ticket() && !starts_with_ticket(words) {
        return Some("This team puts the ticket number first: for example 74-add-login.".to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_become_words_joined_by_dashes() {
        assert_eq!(slugify("Add login to the VIP tab!"), "add-login-to-the-vip-tab");
        assert_eq!(slugify("  fix:  crash on   start-up (again) "), "fix-crash-on-start-up-again");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("ការកែប្រែ"), "", "only letters and digits that a branch name can hold");
        let long = slugify("a very long title that goes on and on and on and on and on");
        assert!(long.len() <= 40 && !long.ends_with('-'), "{long}");
        assert_eq!(long, "a-very-long-title-that-goes-on-and-on", "cut at a word, not in the middle of one");
    }

    #[test]
    fn a_ticket_keeps_its_letters_and_digits_and_drops_the_hash() {
        assert_eq!(ticket_text("#74"), "74");
        assert_eq!(ticket_text(" ABC 123 "), "ABC-123");
        assert_eq!(ticket_text("74/../x"), "74x");
        assert_eq!(ticket_text(""), "");
    }

    #[test]
    fn a_name_is_built_in_the_team_shape() {
        use Shape::*;
        assert_eq!(branch_name(TypeTicketSlug, "feature", "74", "Driver reporting"), "feature/74-driver-reporting");
        assert_eq!(branch_name(TypeTicketSlug, "feature", "", "Driver reporting"), "feature/driver-reporting", "no ticket, none in the name");
        assert_eq!(branch_name(TypeTicketSlug, "bugfix", "ABC-9", ""), "bugfix/ABC-9", "a ticket alone is enough");
        assert_eq!(branch_name(TypeSlug, "feat", "74", "Driver reporting"), "feat/driver-reporting", "this shape has no ticket");
        assert_eq!(branch_name(TicketSlug, "feature", "74", "Driver reporting"), "74-driver-reporting");
        assert_eq!(branch_name(Slug, "feature", "74", "Driver reporting"), "driver-reporting");
        assert_eq!(branch_name(TypeSlug, "feature", "", ""), "", "nothing to name it by");
        assert_eq!(branch_name(TypeSlug, "", "", "Hello"), "hello", "no type chosen");
        assert_eq!(Shape::TypeTicketSlug.example("feature"), "feature/74-add-login");
    }

    #[test]
    fn shapes_are_kept_by_id_and_read_back() {
        for shape in Shape::ALL {
            assert_eq!(Shape::from_id(shape.id()), Some(shape));
        }
        assert_eq!(Shape::from_id("nonsense"), None);
    }

    #[test]
    fn a_ticket_is_a_number_or_a_tracker_key_at_the_start() {
        for yes in ["74-add-login", "74", "ABC-74-add-login", "ABC-74", "PROJ-1-x"] {
            assert!(starts_with_ticket(yes), "{yes}");
        }
        for no in ["add-login", "v2-redesign", "X-1", "2fa", "add-74-things", "proj-1-x", ""] {
            assert!(!starts_with_ticket(no), "{no:?}");
        }
    }

    #[test]
    fn the_habit_is_read_from_the_branches() {
        let branches = [
            "main",
            "develop",
            "feature/74-driver-reporting",
            "feature/75-booking",
            "feature/81-vip-qr",
            "bugfix/90-crash",
            "bugfix/91-typo",
            "release/1.0.0",
            "release/1.1.0",
            "dependabot/npm/lodash",
            "dependabot/npm/react",
            "hotfix",
        ];
        let found = detect(&branches, &["develop", "develop", "develop", "main"], 4, 0, &["develop", "main"]);
        assert_eq!(found.shape, Shape::TypeTicketSlug);
        assert_eq!(found.types[0], ("feature".to_owned(), 3));
        let tied = detect(&["feature/a", "feature/b", "bugfix/a", "bugfix/b", "docs/a", "docs/b"], &[], 0, 0, &[]);
        let order: Vec<&str> = tied.types.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(order, ["feature", "bugfix", "docs"], "equally common: the usual order");
        assert!(found.types.iter().any(|(k, _)| k == "bugfix") && found.types.iter().any(|(k, _)| k == "release"));
        assert!(!found.types.iter().any(|(k, _)| k == "dependabot"), "a bot is not the team");
        assert_eq!(found.base.as_deref(), Some("develop"), "where most branches went");
        assert_eq!(found.merge_style, MergeStyle::Merge);
        let said = found.describe();
        assert!(said.contains("feature/74-add-login") && said.contains("merged into develop") && said.contains("merge commit"), "{said}");
    }

    #[test]
    fn branches_without_tickets_give_the_type_and_title_shape() {
        let branches = ["main", "feat/vip-delivery-report", "feat/redesign-booking", "docs/status-report", "docs/screenshots", "release/1.0.0"];
        let found = detect(&branches, &["release/1.0.0", "release/1.0.0"], 0, 0, &["main"]);
        assert_eq!(found.shape, Shape::TypeSlug);
        assert_eq!(found.base.as_deref(), Some("release/1.0.0"));
        assert_eq!(found.merge_style, MergeStyle::Unknown);
    }

    #[test]
    fn plain_names_and_empty_repositories_do_not_break_it() {
        let found = detect(&["main", "74-fix", "75-feature"], &[], 0, 3, &["main"]);
        assert_eq!(found.shape, Shape::TicketSlug);
        assert_eq!(found.base.as_deref(), Some("main"), "no merges seen: the trunk");
        assert_eq!(found.merge_style, MergeStyle::Squash);
        let none = detect(&[], &[], 0, 0, &[]);
        assert_eq!((none.shape, none.base.clone(), none.types.len()), (Shape::TypeSlug, None, 0));
        assert!(none.describe().contains("no branches"));
        assert_eq!(detect(&["main", "a/b"], &[], 1, 1, &[]).merge_style, MergeStyle::Mixed);
    }

    #[test]
    fn a_name_is_checked_against_habit_not_against_git() {
        let types = vec!["feature".to_owned(), "bugfix".to_owned()];
        let shape = Shape::TypeTicketSlug;
        assert_eq!(name_problem(shape, &types, "feature/74-driver-reporting"), None);
        assert_eq!(name_problem(shape, &types, ""), None, "nothing typed yet is not wrong");
        assert!(name_problem(shape, &types, "driver-reporting").unwrap().contains("type"));
        assert!(name_problem(shape, &types, "chore/74-x").unwrap().contains("chore"));
        assert!(name_problem(shape, &types, "feature/driver-reporting").unwrap().contains("ticket"));
        assert!(name_problem(shape, &types, "feature/74-Driver_Reporting").unwrap().contains("lowercase"));
        assert_eq!(name_problem(shape, &types, "bugfix/ABC-9-crash"), None, "a tracker key keeps its capitals");
        assert_eq!(name_problem(Shape::Slug, &[], "driver-reporting"), None);
        assert!(name_problem(Shape::TypeSlug, &[], "driver-reporting").unwrap().contains("feature/add-login"));
    }
}
