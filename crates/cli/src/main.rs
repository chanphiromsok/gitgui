use std::path::PathBuf;
use std::process::ExitCode;

use gitgui_core::{
    Backend, Commit, Evidence, GitCli, Half, LabelKind, LaneLayout, LogOptions, Row, commit_branches, commit_rank, labels,
    scan_inputs,
};

const USAGE: &str = "usage: gitgui log [PATH] [-n COUNT]\n       gitgui merges [PATH]   which branches are already merged, squash merges included";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("log") => match log(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("gitgui: {message}");
                ExitCode::FAILURE
            }
        },
        Some("merges") => match merges(args.get(1).map_or(".", String::as_str)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("gitgui: {message}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn merges(path: &str) -> Result<(), String> {
    let git = GitCli::new(path);
    let commits = git.log(&LogOptions { max_count: Some(20_000), skip: 0 }).map_err(|e| e.to_string())?;
    let (branches, targets) = scan_inputs(&commits);
    println!(
        "{} branches checked against {}",
        branches.len(),
        targets.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
    );
    let started = std::time::Instant::now();
    let scan = git.merge_clues(&branches, &targets).map_err(|e| e.to_string())?;
    let clues = scan.clues;
    for clue in &clues {
        let how = match clue.evidence {
            Evidence::Contained => "merged".to_owned(),
            Evidence::SamePatch => "squash-merged (identical changes)".to_owned(),
            Evidence::PullRequest => "probably squash-merged (same pull request)".to_owned(),
            Evidence::Messages => "probably squash-merged (repeats its commit messages)".to_owned(),
        };
        let at = clue.commit.as_deref().map(|c| format!(" in {}", &c[..7.min(c.len())])).unwrap_or_default();
        let pr = clue.pr.map(|n| format!(" #{n}")).unwrap_or_default();
        println!("  {:<40} {how} into {}{at}{pr}", clue.branch, clue.into);
    }
    let merged: std::collections::HashSet<&str> = clues.iter().map(|c| c.branch.as_str()).collect();
    for branch in branches.iter().filter(|b| !merged.contains(b.name.as_str())) {
        println!("  {:<40} no sign of a merge", branch.name);
    }
    if scan.unchecked > 0 {
        println!("{} branches were not checked: the scan ran out of time, so say nothing about them", scan.unchecked);
    }
    println!("({:.1} s)", started.elapsed().as_secs_f64());
    Ok(())
}

fn log(args: &[String]) -> Result<(), String> {
    let mut path = PathBuf::from(".");
    let mut count = 50usize;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-n" => {
                let value = args.next().ok_or(USAGE)?;
                count = value.parse().map_err(|_| format!("-n wants a number, got {value:?}"))?;
            }
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}\n{USAGE}")),
            _ => path = PathBuf::from(arg),
        }
    }

    let commits = GitCli::new(path)
        .log(&LogOptions { max_count: Some(count), skip: 0 })
        .map_err(|err| err.to_string())?;

    let mut layout = LaneLayout::new();
    for commit in &commits {
        let row = layout.push_branch(&commit.id, &commit.parents, commit_rank(commit), &commit_branches(commit));
        println!("{}", render(commit, &row));
    }
    Ok(())
}

/// One line per commit: the lane drawing, then hash, refs and subject.
fn render(commit: &Commit, row: &Row) -> String {
    let mut graph = String::new();
    for col in 0..row.width {
        let through = row.strokes.iter().any(|s| s.half == Half::Through && s.from == col);
        let joins_in = row.strokes.iter().find(|s| s.half == Half::Top && s.from == col && col != row.lane);
        let leaves_to = row.strokes.iter().find(|s| s.half == Half::Bottom && s.to == col && col != row.lane);
        // A line that turns into this column at the commit's row is shown as the turn, even where the column's own
        // line carries on through the row.
        graph.push(if col == row.lane {
            '*'
        } else if joins_in.is_some() {
            if col > row.lane { '/' } else { '\\' }
        } else if leaves_to.is_some() {
            if col > row.lane { '\\' } else { '/' }
        } else if through {
            '|'
        } else {
            ' '
        });
        graph.push(' ');
    }

    let refs: Vec<String> = labels(&commit.refs)
        .iter()
        .map(|label| {
            let mut text = match label.kind {
                LabelKind::Tag => format!("tag:{}", label.name),
                _ => label.name.clone(),
            };
            for remote in &label.remotes {
                text.push_str(&format!(" | {remote}"));
            }
            if label.head { format!("*{text}") } else { text }
        })
        .collect();
    let refs = if refs.is_empty() { String::new() } else { format!(" ({})", refs.join(", ")) };

    // Pad the drawing so the hashes line up even as lanes come and go.
    format!("{graph:<12} {}  {:<16}{refs} {}", commit.short_id(), commit.date, commit.summary)
}
