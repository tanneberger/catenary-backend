## Architecture

Catenary Backend is a distributed system comprised of microservices operating in Kubernetes. The system is designed for fault tolerance, high-avaliability, and native execution speed in x86-64 using the Rust systems programming language.

- **Maple**: GTFS Downloader and ingestion engine
- **Prairie**: Routing Preprocessor and execution engine (Research and design in progress)
- **Alpenrose**: Distributed system to ingest GTFS-rt and other realtime data (Rose des Alpes), successor to Kactus.
- **Aspen**: Processing of realtime data and dynamic insertion into other engines
- **Edelweiss**: Map tile geometry server, will serve line ordering optimised graph maps (LOOM) in the future.
- **Spruce**: Websocket server for frontend to stream data to and from backend, including realtime locations, stop times
- **Birch**: General API server

The kubernetes configuration is generated using Helm templates. See Helm's documentation for further information on that.

The code is heavily commented, go to each folder in src for more information.

### Submodules maintained 
- **Dmfr folder reader**: reads data from transitland-atlas into raw structs
- **Château**: Associates feeds with operators and vise versa using depth first search in knowledge graph
- **Amtrak GTFS rt**: Conversion of proprietary realtime data from amtrak's website into gtfs-rt.
- **Zotgtfs**: conversion of Transloc data and hand typed schedules from Anteater Express to GTFS schedule and realtime.

## Install Dependencies

```bash
sudo apt install protobuf-compiler build-essential gcc pkg-config libssl-dev postgresql unzip wget
```

## For Contributors

For unix users, running `git config core.hooksPath .githooks` is recommended.
Good commit messages are required to contribute to this project.

No option exists for Windows users at the moment. Please try WSL Ubuntu for the moment. We're working on adding this.

### Installation of Postgres

See https://www.postgresql.org/download

PostGIS is also required like 
```bash
sudo apt install postgresql-16-postgis-3
```

See https://trac.osgeo.org/postgis/wiki/UsersWikiPostGIS3UbuntuPGSQLApt for more instructions

### SQL notes
We've switched to diesel for our queries. Read the diesel documentation to learn how to use it.
https://diesel.rs/guides/getting-started.html

Lib PQ is also required to install the diesel cli. Only postgres is required.
Example
```bash
sudo apt-get install libpq-dev
cargo install diesel_cli --no-default-features --features postgres
```

### Common Database debugging

Is Postgis not installing? This page may be helpful: https://trac.osgeo.org/postgis/wiki/UsersWikiPostGIS3UbuntuPGSQLApt

### Updating transitland submodules

```bash
git submodule update --rebase --remote
```

### Style Guide

Code should be formatted with `cargo fmt` and be well documented.
The following `cargo clippy` rules are enforced.

```rs
#![deny(
    clippy::mutable_key_type,
    clippy::map_entry,
    clippy::boxed_local,
    clippy::assigning_clones,
    clippy::redundant_allocation,
    bool_comparison,
    bind_instead_of_map,
    clippy::vec_box,
    clippy::while_let_loop,
    useless_asref,
    clippy::repeat_once,
    clippy::deref_addrof,
    clippy::suspicious_map,
    clippy::arc_with_non_send_sync,
    clippy::single_char_pattern,
    clippy::for_kv_map
)]
```