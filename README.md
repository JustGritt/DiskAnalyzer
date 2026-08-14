# DiskAnalyzer

A read-only inventory of what is actually using space on your drive, with the
live Windows installation left out of the way so the things you *can* act on
are the things you see.

It reads. That is all it does. There is no code path in this project that
deletes, moves, renames or modifies a file, and the tool verifies that about
itself before every scan.

```
python analyze.py
```

Requires Python 3.10+. No dependencies, no installation.

---

## What you get

A **terminal report** with the volume gauge, where the space is, what kind of
thing it is, the largest files, and the large files you have not touched in
over a year.

An **HTML report** (`report.html`) with an interactive treemap you can click
into, plus sortable and filterable tables. One self-contained file: no network,
no companion assets. Open it, move it, mail it to yourself; it still works.

**CSV listings** (`files.csv`, `folders.csv`) for the questions the reports do
not answer. Sort every file you own by size, filter to one extension, hand the
list to another tool.

Reports land in `reports/scan-<timestamp>/` unless you pass `--out`.

---

## The read-only guarantee

The promise is not "we were careful". `diskanalyzer/safety.py` reads this
package's own source and looks for the calls that could do damage — `os.remove`,
`os.unlink`, `shutil.rmtree`, `os.rename`, `os.truncate`, `open()` in a write
mode outside the report writers, and others. The audit runs before every scan
and aborts the run if anything turns up, so a careless future edit fails loudly
instead of quietly gaining the ability to delete your files.

Check it yourself at any time:

```
python analyze.py --verify-read-only
```

The only files this project ever opens for writing are the reports, in the
output directory you chose. Scanned files are opened read-only, and only when
`--duplicates` needs to compare their contents.

---

## Usage

```
python analyze.py                          scan C:, skipping the live OS
python analyze.py --duplicates             also find duplicate files
python analyze.py D:\ E:\                  scan other drives
python analyze.py --csv-min-size 0         list every file, not just large ones
python analyze.py --help                   all options
```

### Scope

| Option | Effect |
| --- | --- |
| `--include-os` | also scan `C:\Windows` and the other live OS folders |
| `--exclude PATH` | skip a folder (repeatable) |
| `--exclude-name NAME` | skip any folder with this name (repeatable) |

### Sizing

| Option | Effect |
| --- | --- |
| `--allocated` | estimate on-disk size by rounding files up to whole clusters |
| `--cluster-size BYTES` | cluster size for `--allocated` (default 4096) |

### Detail

| Option | Effect |
| --- | --- |
| `--top N` | folders per ranking (default 25) |
| `--files N` | files per ranking (default 30) |
| `--old-min-size SIZE` | size floor for the untouched-files list (default 100MB) |
| `--old-min-days DAYS` | age floor for the untouched-files list (default 365) |
| `--duplicates` | look for duplicate files |
| `--dup-min-size SIZE` | ignore duplicates below this size (default 25MB) |

### Output

| Option | Effect |
| --- | --- |
| `--out DIR` | report directory |
| `--csv-min-size SIZE` | size floor for `files.csv`; `0` lists every file (default 10MB) |
| `--folder-min-size SIZE` | size floor for `folders.csv`; `0` lists every folder (default 1MB) |
| `--no-html`, `--no-csv` | skip an output format |
| `--no-color`, `--quiet` | plain text; no progress line |

Sizes accept `500MB`, `2GB`, `1.5g` or a plain byte count.

---

## What is skipped, and why

The goal is "everything except the operating system", which is a narrower cut
than "everything made by Microsoft". Leftovers like `C:\Windows.old` and the
Recycle Bin are Microsoft-made but genuinely reclaimable, so they stay in the
scan and get their own categories.

Skipped by default (`--include-os` to include them):

- `C:\Windows`, `C:\Program Files\WindowsApps`, the Defender folders,
  `C:\Recovery`, `C:\PerfLogs`, `C:\Config.Msi`, `C:\$WinREAgent`
- `C:\System Volume Information` — restore points and shadow copies, readable
  only as SYSTEM. If this matters to you, `vssadmin list shadowstorage` will
  tell you how big it is.
- Legacy compatibility junctions that loop back into real folders

Never followed anywhere: junctions, symlinks and mounted volumes. They are
counted in the report as links but not descended into, because following them
would double-count (`AppData\Local\Application Data` points at its own parent)
or silently wander onto a different disk.

Volume-level files Windows sizes for itself — `pagefile.sys`, `hiberfil.sys`,
`swapfile.sys` — are reported on their own line instead of being mixed into
folder totals. They can be several gigabytes each, but the way to change them
is System settings, never deletion.

---

## How space is classified

Knowing that your user folder holds 300 GB is not actionable. Knowing that
84 GB of it is regenerable build output is. Every file is sorted into a
category carrying one of three confidence levels:

- **safe** — regenerable or already discarded. Removing it costs you time, not
  data: package caches, build output, `node_modules`, temp folders, the
  Recycle Bin.
- **review** — very often unwanted, but only you know: Downloads, installer
  caches, ISOs and archives, virtual disks, `Windows.old`.
- **info** — not junk, reported so the total adds up and you can see the shape:
  installed applications, games, media, cloud-synced folders.

Classification is by folder, inherited downward, so one match covers a whole
tree. A small set of override rules is checked at every level, because a broad
rule high in the tree would otherwise hide something specific below it — a
Python environment nested under `AppData\Local\Programs` is restorable
dependencies, not an installed application.

These labels say where looking is worthwhile. Nothing more. DiskAnalyzer has no
ability to act on any of them.

---

## Duplicate detection

`--duplicates` groups files by exact size, then compares the first and last
64 KB of each candidate. A group of 40 GB virtual disks therefore costs a few
megabytes of reading rather than 40 GB.

Matching size, head and tail is strong evidence of a duplicate but it is not
proof, which is why the report says *likely* and why nothing acts on the
finding. Check before you decide anything. The search stops at a read budget
and says so when it does.

---

## Accuracy notes

**Logical vs on-disk size.** By default the report shows logical file sizes,
the same number Explorer's "Size" column shows. Actual disk usage differs for
compressed files (less), sparse files (much less) and for the large number of
tiny files that each still occupy a whole cluster (more). `--allocated`
approximates the last of these by rounding up to the cluster size; it does not
detect NTFS compression.

**Hard links.** A file with several hard links is counted once per link, so a
tree using them reads as larger than it is. Windows uses hard links heavily
inside `C:\Windows`, which is skipped by default.

**Unreadable paths.** Some folders need Administrator rights. They are counted,
listed in `unreadable.csv`, and reported as a count so the totals can be read
honestly rather than looking complete when they are not. Running an elevated
terminal reaches more of them.

**Coverage.** The summary states what fraction of the volume's used space the
scan accounted for. The remainder is the skipped OS folders, unreadable paths,
and the differences above.

---

## Project layout

```
analyze.py                  entry point
diskanalyzer/
  safety.py                 the read-only audit
  scanner.py                the single-pass walk
  exclusions.py             what is skipped, and why
  categories.py             classification rules
  insights.py               rankings, duplicates, treemap pruning
  report_terminal.py        the terminal report
  report_html.py            the self-contained HTML report
  report_csv.py             the CSV listings
  cli.py                    argument parsing and wiring
```

### How it stays fast

A full 2.5 million file, 386 GB scan of a C: drive takes about 40 seconds.

Everything is accumulated in one pass; nothing re-reads the disk afterwards.
Per-file records are never kept — a drive that size would cost gigabytes of
memory — so the walk holds one node per *directory* and streams file data into
fixed-size structures: a top-N heap, an extension histogram, age buckets, and
the CSV writer. On Windows the directory enumeration already carries size and
timestamps, so reading them costs no extra system call.
