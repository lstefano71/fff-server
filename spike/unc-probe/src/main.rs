// Step 0 spike. Throwaway: answers whether fff-search 0.11 can index a UNC root on Windows,
// and whether the facts fff-server's design depends on actually hold. Delete after step 1.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fff_search::file_picker::FilePicker;
use fff_search::frecency::FrecencyTracker;
use fff_search::grep::{Casing, GrepMode, GrepSearchOptions};
use fff_search::query_tracker::QueryTracker;
use fff_search::{
    FFFMode, FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser, SharedFilePicker,
    SharedFrecency, SharedQueryTracker,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = match args.next() {
        Some(r) => r,
        None => {
            eprintln!("usage: unc-probe <path> [fuzzy-query] [grep-pattern]");
            eprintln!(r"  e.g. unc-probe \\server\share\project foo TODO");
            std::process::exit(2);
        }
    };
    let fuzzy_query = args.next().unwrap_or_else(|| "a".into());
    let grep_pattern = args.next().unwrap_or_else(|| "the".into());
    // The watcher check has to create a file in the target tree. Skip it for read-only
    // shares, or when probing a tree you would rather not touch.
    let no_write = std::env::args().any(|a| a == "--no-write");

    println!("== fff-search 0.11 UNC / Windows probe ==\n");
    println!("[input]");
    println!("  argument         : {root:?}");
    println!("  byte length      : {}", root.len());

    // 1. What does canonicalisation do to this path? The design keys workspaces on the
    //    canonical form, so this is the first thing that has to behave.
    match std::fs::canonicalize(&root) {
        Ok(p) => println!("  std::canonicalize : {p:?}"),
        Err(e) => println!("  std::canonicalize : FAILED: {e}"),
    }
    match dunce::canonicalize(&root) {
        Ok(p) => println!("  dunce::canonicalize: {p:?}"),
        Err(e) => println!("  dunce::canonicalize: FAILED: {e}"),
    }
    let as_path = Path::new(&root);
    println!("  exists           : {}", as_path.exists());
    println!(
        "  parent           : {:?}  (None => fff rejects it as a filesystem root)",
        as_path.parent()
    );

    // 2. Bring up the picker exactly as the server will.
    let tmp = std::env::temp_dir().join("fff-unc-probe");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();
    let shared_query_tracker = SharedQueryTracker::default();

    println!("\n[databases]");
    match FrecencyTracker::open(tmp.join("frecency")) {
        Ok(f) => match shared_frecency.init(f) {
            Ok(()) => println!("  frecency          : ok"),
            Err(e) => println!("  frecency init     : FAILED: {e}"),
        },
        Err(e) => println!("  frecency open     : FAILED: {e}"),
    }
    match QueryTracker::open(tmp.join("queries")) {
        Ok(q) => match shared_query_tracker.init(q) {
            Ok(()) => println!("  query tracker     : ok"),
            Err(e) => println!("  query tracker init: FAILED: {e}"),
        },
        Err(e) => println!("  query tracker open: FAILED: {e}"),
    }

    println!("\n[scan]");
    let started = Instant::now();
    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: root.clone(),
            mode: FFFMode::Ai,
            enable_content_indexing: true,
            watch: true,
            ..Default::default()
        },
    )?;

    // A network share may be far slower than a local disk; allow generous headroom.
    let completed = shared_picker.wait_for_scan(Duration::from_secs(120));
    println!("  wait_for_scan     : {completed}  ({:?})", started.elapsed());

    // wait_for_scan only means files are searchable. The watcher and the content index
    // come up afterwards, so anything testing freshness has to wait for them separately.
    let indexed = shared_picker.wait_for_indexing_complete(Duration::from_secs(120));
    println!("  indexing complete : {indexed}  ({:?})", started.elapsed());
    let watcher_up = shared_picker.wait_for_watcher(Duration::from_secs(60));
    println!("  watcher up        : {watcher_up}  ({:?})", started.elapsed());

    let guard = shared_picker.read()?;
    let picker = match guard.as_ref() {
        Some(p) => p,
        None => {
            println!("  picker            : ABSENT after scan — nothing else can be probed");
            return Ok(());
        }
    };

    let progress = picker.get_scan_progress();
    println!("  scanned files     : {}", progress.scanned_files_count);
    println!("  is_scanning       : {}", progress.is_scanning);
    println!("  watcher ready     : {}", progress.is_watcher_ready);
    println!("  warmup complete   : {}", progress.is_warmup_complete);
    println!("  live_file_count   : {}", picker.live_file_count());

    // 3. The stored base_path is the identity key the design relies on. Does it keep a
    //    verbatim \\?\ prefix on a UNC root, or is it simplified?
    println!("\n[paths]");
    let base = picker.base_path();
    println!("  stored base_path  : {base:?}");
    let base_str = base.to_string_lossy();
    println!(
        "  verbatim prefix   : {}  (true => breaks the git-workdir comparison)",
        base_str.starts_with(r"\\?\")
    );
    println!("  round-trips       : {}", base == as_path);
    println!("  has_git_repo      : {}", picker.has_git_repo());
    println!("  git_root          : {:?}", picker.git_root());

    // Relative paths must be forward-slashed; absolute must be usable.
    let files = picker.get_files();
    println!("\n  first indexed files (relative / absolute):");
    let mut shown = 0;
    let mut sample: Option<String> = None;
    for f in files.iter() {
        if f.is_deleted() {
            continue;
        }
        let rel = f.relative_path(picker);
        if shown < 5 {
            println!("    rel  {rel:?}");
            println!("    abs  {:?}", f.absolute_path(picker, base));
            println!(
                "    git  {:?}  size {}  frecency {}  git_recency {}",
                f.git_status,
                f.size,
                f.total_frecency_score(),
                f.git_recency_score
            );
        }
        if sample.is_none() && !rel.is_empty() {
            sample = Some(rel.clone());
        }
        shown += 1;
        if shown >= 5 && sample.is_some() {
            break;
        }
    }
    let backslashed = files
        .iter()
        .filter(|f| !f.is_deleted())
        .take(2000)
        .filter(|f| f.relative_path(picker).contains('\\'))
        .count();
    println!(
        "  relpaths with '\\' : {backslashed}  (expect 0 — index should be '/'-canonical)"
    );

    // 4. Search, with the full 0.11 score breakdown.
    println!("\n[fuzzy search: {fuzzy_query:?}]");
    let parser = QueryParser::default();
    let query = parser.parse(&fuzzy_query);
    let qt = shared_query_tracker.read()?;
    let t = Instant::now();
    let results = picker.fuzzy_search(
        &query,
        qt.as_ref(),
        FuzzySearchOptions {
            max_threads: 0,
            project_path: Some(base),
            pagination: PaginationArgs {
                offset: 0,
                limit: 5,
            },
            ..Default::default()
        },
    );
    println!(
        "  total_matched {} / total_files {}  ({:?})",
        results.total_matched,
        results.total_files,
        t.elapsed()
    );
    for (i, item) in results.items.iter().enumerate().take(3) {
        let s = &results.scores[i];
        println!("    {:?}", item.relative_path(picker));
        println!(
            "      total {} base {} filename {} special {} frecency {} git_status {} git_recency {}",
            s.total,
            s.base_score,
            s.filename_bonus,
            s.special_filename_bonus,
            s.frecency_boost,
            s.git_status_boost,
            s.git_recency_boost
        );
        println!(
            "      distance {} current_file {} combo {} path_align {} exact {} type {:?}",
            s.distance_penalty,
            s.current_file_penalty,
            s.combo_match_boost,
            s.path_alignment_bonus,
            s.exact_match,
            s.match_type
        );
        println!("      ranges {:?}", results.match_byte_offsets[i]);
    }

    // 5. Grep, including the abort_signal the server needs for cancellation.
    println!("\n[grep: {grep_pattern:?}]");
    let gq = QueryParser::default().parse(&grep_pattern);
    let abort = Arc::new(AtomicBool::new(false));
    let t = Instant::now();
    let grep = picker.grep(
        &gq,
        &GrepSearchOptions {
            mode: GrepMode::PlainText,
            casing: Some(Casing::Smart),
            page_limit: 10,
            classify_definitions: true,
            before_context: 1,
            after_context: 1,
            abort_signal: Some(abort.clone()),
            ..Default::default()
        },
    );
    println!(
        "  matches {} in {} files / searched {} / filtered {} ({:?})",
        grep.matches.len(),
        grep.files_with_matches,
        grep.total_files_searched,
        grep.filtered_file_count,
        t.elapsed()
    );
    println!("  next_file_offset  : {}", grep.next_file_offset);
    println!("  literal_fallback  : {}", grep.literal_fallback);
    println!("  regex_fallback    : {:?}", grep.regex_fallback_error);
    for m in grep.matches.iter().take(3) {
        let file = grep.files[m.file_index];
        println!(
            "    {}:{}:{} def={} {:?}",
            file.relative_path(picker),
            m.line_number,
            m.col,
            m.is_definition,
            m.line_content.chars().take(70).collect::<String>()
        );
        println!("      ranges {:?}  byte_offset {}", m.match_byte_offsets, m.byte_offset);
    }

    // Does the abort flag actually stop a search? Run the same wide grep twice with
    // identical options — once clean, once with the flag already set — and compare.
    let wide = |signal: Option<Arc<AtomicBool>>| {
        let t = Instant::now();
        let r = picker.grep(
            &gq,
            &GrepSearchOptions {
                mode: GrepMode::PlainText,
                page_limit: 1_000_000,
                abort_signal: signal,
                ..Default::default()
            },
        );
        (r.total_files_searched, r.matches.len(), t.elapsed())
    };
    println!("\n[abort_signal]");
    // This greps the whole tree twice. Over SMB at ~7ms/file that is many minutes, so skip
    // it on big trees unless explicitly asked. It is a local-disk sanity check anyway.
    let force_abort_test = std::env::args().any(|a| a == "--abort-test");
    if grep.filtered_file_count > 5_000 && !force_abort_test {
        println!(
            "  skipped: {} searchable files is too many to grep twice over a slow link",
            grep.filtered_file_count
        );
        println!("  pass --abort-test to run it anyway");
    } else {
        let (clean_files, clean_matches, clean_time) = wide(None);
        let preset = Arc::new(AtomicBool::new(true));
        let (abort_files, abort_matches, abort_time) = wide(Some(preset));
        println!("  clean   : {clean_files} files, {clean_matches} matches, {clean_time:?}");
        println!("  aborted : {abort_files} files, {abort_matches} matches, {abort_time:?}");
        println!(
            "  verdict : {}",
            if abort_files < clean_files || abort_matches < clean_matches {
                "abort_signal takes effect"
            } else {
                "NO observable effect (tree may be too small to show it)"
            }
        );
    }

    drop(qt);
    drop(guard);

    // 6. Watcher freshness. On SMB, ReadDirectoryChangesW may never deliver.
    println!("\n[watcher]");
    if no_write {
        println!("  skipped (--no-write)");
        let _ = std::fs::remove_dir_all(&tmp);
        println!("\n== done ==");
        return Ok(());
    }
    if !watcher_up {
        println!("  watcher never became ready — freshness test below is expected to fail");
    }
    let probe_name = format!("fff-probe-{}.txt", std::process::id());
    let probe_file = as_path.join(&probe_name);
    match std::fs::write(&probe_file, b"fff-server watcher probe\n") {
        Ok(()) => {
            println!("  wrote             : {probe_name}");
            let mut seen = false;
            for attempt in 1..=20 {
                std::thread::sleep(Duration::from_millis(500));
                let g = shared_picker.read()?;
                if let Some(p) = g.as_ref() {
                    let q = QueryParser::default().parse(&probe_name);
                    let r = p.fuzzy_search(
                        &q,
                        None,
                        FuzzySearchOptions {
                            pagination: PaginationArgs {
                                offset: 0,
                                limit: 5,
                            },
                            ..Default::default()
                        },
                    );
                    if r.items
                        .iter()
                        .any(|f| f.relative_path(p).ends_with(&probe_name))
                    {
                        println!("  indexed after     : ~{} ms", attempt * 500);
                        seen = true;
                        break;
                    }
                }
            }
            if !seen {
                println!("  NOT indexed within 10s — watcher is not delivering on this root");
                println!("  (this is the case the design's periodic rescan exists for)");
            }
            let _ = std::fs::remove_file(&probe_file);
        }
        Err(e) => println!("  could not write probe file (read-only share?): {e}"),
    }

    let _ = std::fs::remove_dir_all(&tmp);
    println!("\n== done ==");
    Ok(())
}
