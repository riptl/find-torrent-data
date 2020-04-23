#[macro_use]
extern crate clap;

use std::borrow::Borrow;
use std::error::Error;
use std::fs::{create_dir_all, hard_link, File};
use std::io::{Read, Result as IOResult, Seek, SeekFrom};
use std::iter::Iterator;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lava_torrent::torrent::v1::Torrent;
use multimap::MultiMap;
use sha1::{Digest, Sha1};
use walkdir::WalkDir;

fn main() {
    let cli = clap_app!(myapp =>
        (name: "find-torrent-data")
        (version: "1.0")
        (author: "Richard Patel <me@terorie.dev>")
        (about: "Search for files that are part of a torrent and prepare a directory with links to these files")
        (@arg input: -i +takes_value +required +multiple "Add search directory")
        (@arg output: -o +takes_value default_value("./") "Output directory")
        (@arg create_symlinks: -s --symlinks "Use symbolic links")
        (@arg follow_symlinks: --("follow-symlinks") "Follow symlinks in input")
        (@arg hash: -h +takes_value default_value("1.0") "Fraction of hash pieces to be verified")
        (@arg TORRENT: +required "Torrent file")
    ).get_matches();
    if let Err(e) = run(cli) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

macro_rules! unwrap_or_break {
    ($x:expr) => {
        match $x {
            Some(x) => x,
            None => break,
        };
    };
}

#[derive(Clone)]
struct Extent {
    offset: i64,
    size: i64,
    hash: [u8; 20],
}

#[derive(Clone)]
struct Descriptor {
    path: PathBuf,
    size: i64,
    extents: Vec<Extent>,
}

impl Descriptor {
    // Verify the content of a file against the extent hashes in the descriptor.
    // `threshold` is the fraction of correct hashes.
    // For example, if `threshold` is 0.5, the first half must match.
    fn verify_file<T>(&self, file: &mut T, threshold: f32) -> IOResult<bool>
    where
        T: Seek + Read,
    {
        debug_assert!(threshold >= 0.0 && threshold <= 1.0);
        let count = (self.extents.len() as f32 * threshold) as usize;
        for i in 0..count {
            let extent = &self.extents[i];
            // Seek to block
            file.seek(SeekFrom::Start(extent.offset as u64))?;
            // Hash single block
            let mut state = Sha1::new();
            let bytes_hashed = std::io::copy(&mut file.take(extent.size as u64), &mut state)?;
            if bytes_hashed as i64 != extent.size {
                return Ok(false);
            }
            // Compare hashes
            let hash = state.result();
            if hash.as_slice() != &extent.hash[..] {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

struct Match {
    is_path: PathBuf,
    want_path: PathBuf,
}

impl Match {
    fn link(&self, symlink: bool) -> IOResult<()> {
        if let Some(parent) = self.want_path.parent() {
            create_dir_all(parent)?;
        }
        if symlink {
            soft_link(&self.is_path, &self.want_path)
        } else {
            hard_link(&self.is_path, &self.want_path)
        }
    }
}

fn run(cli: clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    // Read torrent file and create hash descriptors
    let output_path = cli.value_of("output").unwrap();
    let output_path = PathBuf::from(output_path);
    let torrent_path = cli.value_of("TORRENT").unwrap();
    let descriptors = make_descriptors(torrent_path, &output_path)
        .map_err(|e| format!("Failed to read torrent: {}", e))?;

    // Lookup descriptors by size
    let by_size: MultiMap<i64, Descriptor> =
        descriptors.iter().map(|d| (d.size, d.clone())).collect();

    // Walk input directories and detect matching file sizes
    let ctx = Rc::new(SearchContext {
        by_size,
        follow_symlinks: cli.is_present("follow_symlinks"),
        create_symlinks: cli.is_present("create_symlinks"),
        hash_threshold: cli.value_of("hash").unwrap().parse::<f32>()?,
    });
    let input_dirs = cli.values_of_lossy("input").unwrap();
    for input_dir in input_dirs {
        for m in search_dir(&input_dir, &ctx) {
            println!(
                "{} <= {}",
                m.want_path.to_string_lossy(),
                m.is_path.to_string_lossy()
            );
            if let Err(e) = m.link(ctx.create_symlinks) {
                eprintln!("{}", e);
            }
        }
    }

    Ok(())
}

struct SearchContext {
    by_size: MultiMap<i64, Descriptor>,
    follow_symlinks: bool,
    create_symlinks: bool,
    hash_threshold: f32,
}

// Searches a directory at path for files that match descriptors in `by_size`.
// If `symlinks` is enabled, files behind symbolic links are also considered.
fn search_dir(path: &str, ctx: &Rc<SearchContext>) -> impl Iterator<Item = Match> {
    let hash_threshold = ctx.hash_threshold;
    let ctx = Rc::clone(ctx);
    WalkDir::new(path)
        .follow_links(ctx.follow_symlinks)
        .into_iter()
        // Print and filter errors
        .filter_map(|entry| entry.map_err(|err| eprintln!("{}", err)).ok())
        // Ignore directories
        .filter(|entry| entry.file_type().is_file())
        // Get metadata
        .filter_map(|entry| {
            entry
                .metadata()
                .map_err(|err| eprintln!("{}", err))
                .ok()
                .map(|meta| (entry, meta))
        })
        // Lookup sizes to get matches
        .filter_map(move |(entry, meta)| {
            let size = meta.len();
            ctx.by_size
                .get(&(size as i64))
                .map(|d| (entry.path().to_path_buf(), d.clone()))
        })
        // Verify hashes
        .filter(move |(path, d)| {
            File::open(path)
                .and_then(|mut file| d.verify_file(&mut file, hash_threshold))
                .unwrap_or_else(|err| {
                    eprintln!("{}", err);
                    false
                })
        })
        // Map to match struct
        .map(|(path, descriptor)| {
            let want_path: &PathBuf = descriptor.path.borrow();
            Match {
                is_path: path,
                want_path: want_path.clone(),
            }
        })
}

fn make_descriptors(
    torrent_path: &str,
    want_prefix: &PathBuf,
) -> Result<Vec<Descriptor>, Box<dyn Error>> {
    let torrent = Torrent::read_from_file(torrent_path)?;
    if let Some(ref files) = torrent.files {
        // Directory torrent
        if files.is_empty() || torrent.pieces.is_empty() {
            return Ok(vec![]);
        }
        let dir_name = want_prefix.join(&torrent.name);
        let mut descriptors = Vec::<Descriptor>::new();
        let mut pieces = torrent.pieces.iter();
        let mut file_offset = 0i64;
        for file in files {
            // If offset exceeds file, skip to next
            if file_offset >= file.length {
                file_offset -= file.length;
                continue;
            }
            let mut extents = Vec::new();
            // Iterate pieces until end of file reached
            while file.length - file_offset >= torrent.piece_length {
                let piece = unwrap_or_break!(pieces.next());
                extents.push(Extent {
                    offset: file_offset,
                    size: torrent.piece_length,
                    hash: unwrap_piece(&piece),
                });
                file_offset += torrent.piece_length;
            }
            // Finalize descriptor
            if !extents.is_empty() {
                descriptors.push(Descriptor {
                    path: dir_name.join(file.path.clone()),
                    extents,
                    size: file.length,
                });
            }
            // Ignore piece that overlaps two files
            if file.length - file_offset > 0 {
                file_offset = torrent.piece_length - (file.length - file_offset);
                unwrap_or_break!(pieces.next());
            }
        }
        Ok(descriptors)
    } else {
        // Single file torrent, collect all pieces and return single descriptor.
        let extents = torrent
            .pieces
            .iter()
            .scan(0i64, |offset, piece| {
                let ext = Extent {
                    offset: *offset,
                    size: torrent.piece_length,
                    hash: unwrap_piece(piece),
                };
                *offset += torrent.piece_length;
                Some(ext)
            })
            .collect();
        let mut path = want_prefix.clone();
        path.push(&torrent.name);
        Ok(vec![Descriptor {
            path,
            size: torrent.length,
            extents,
        }])
    }
}

fn unwrap_piece(piece: &[u8]) -> [u8; 20] {
    let mut array = [0u8; 20];
    let bytes = &piece[..20];
    array.copy_from_slice(bytes);
    array
}

#[cfg(target_family = "windows")]
pub fn soft_link<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> IOResult<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

#[cfg(target_family = "unix")]
pub fn soft_link<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> IOResult<()> {
    std::os::unix::fs::symlink(src, dst)
}
