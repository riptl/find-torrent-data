# find-torrent-data

Reads a `.torrent` file and searches for files with matching content (file name doesn't matter).
The files are then (sym)linked to a directory structure resembling the original torrent,
ready to be loaded to a torrent client for (re-)seeding.

Quick installation: `cargo install find-torrent-data`

Help:

```
find-torrent-data 1.0
Richard Patel <me@terorie.dev>
Search for files that are part of a torrent and prepare a directory with links to these files

USAGE:
    find-torrent-data [FLAGS] [OPTIONS] <TORRENT> -i <input>...

FLAGS:
    -s, --symlinks           Use symbolic links
        --follow-symlinks    Follow symlinks in input
        --help               Prints help information
    -V, --version            Prints version information

OPTIONS:
    -h <hash>            Fraction of hash pieces to be verified [default: 1.0]
    -i <input>...        Add search directory
    -o <output>          Output directory [default: ./]

ARGS:
    <TORRENT>    Torrent file
```
