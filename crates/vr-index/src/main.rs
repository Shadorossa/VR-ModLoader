//! `vr-index` — build and query the GAME INDEX from the command line (docs/app/vr-index.md §5).

use std::path::PathBuf;
use std::process::ExitCode;

use vr_index::source::{GameSource, SourceOptions};
use vr_index::{lang_index, BuildOptions, Category, Index, Query};

const USAGE: &str = "vr-index — índice del juego para VR-ModLoader Studio

  vr-index build   --game <carpeta del juego> [--out <carpeta>] [--no-thumbs] [--no-texts] [--no-audio]
                   [--thumb-size N] [--include-mods] [--cpk-list <cpk_list.cfg.bin>] [--force]
  vr-index status  --game <carpeta> [--out <carpeta>]
  vr-index info    [--out <carpeta>]
  vr-index search  <texto> [--cat character,technique…] [--lang es] [--limit N] [--field k=v] [--out …] [--json]
  vr-index get     <id> [--cat <categoría>] [--lang es] [--out …]            (JSON)
  vr-index resolve <nombre> [--cat …] [--lang es] [--out …]                 (JSON: unique / ambiguous / not_found)
  vr-index text    <archivo> <id> [--out …]                                 (las 9 lenguas)
  vr-index ls      --game <carpeta> <prefijo de ruta>                        (depuración: rutas de cpk_list)
  vr-index peek    --game <carpeta> <ruta | carpeta/raíz_> [LISTA…]          (depuración: filas de un cfg.bin)

--out por defecto: %LOCALAPPDATA%\\VR-ModLoader\\index";

struct Args {
    pos: Vec<String>,
    opts: std::collections::HashMap<String, Vec<String>>,
    flags: std::collections::HashSet<String>,
}

fn parse() -> Args {
    let mut a = Args { pos: Vec::new(), opts: Default::default(), flags: Default::default() };
    let mut it = std::env::args().skip(1).peekable();
    const WITH_VALUE: [&str; 9] = ["--game", "--out", "--cat", "--lang", "--limit", "--field", "--thumb-size", "--cpk-list", "--category"];
    while let Some(x) = it.next() {
        if WITH_VALUE.contains(&x.as_str()) {
            if let Some(v) = it.next() {
                a.opts.entry(x).or_default().push(v);
            }
        } else if x.starts_with("--") {
            a.flags.insert(x);
        } else {
            a.pos.push(x);
        }
    }
    a
}

impl Args {
    fn one(&self, k: &str) -> Option<&str> {
        self.opts.get(k).and_then(|v| v.last()).map(String::as_str)
    }
    fn out(&self) -> PathBuf {
        self.one("--out").map(PathBuf::from).unwrap_or_else(vr_index::default_out_dir)
    }
    fn lang(&self) -> usize {
        self.one("--lang").and_then(lang_index).unwrap_or(1)
    }
    fn cats(&self) -> Vec<Category> {
        self.opts
            .get("--cat")
            .into_iter()
            .chain(self.opts.get("--category"))
            .flatten()
            .flat_map(|s| s.split(','))
            .filter_map(|c| {
                let r = Category::parse(c.trim());
                if r.is_none() {
                    eprintln!("categoría desconocida: {c}");
                }
                r
            })
            .collect()
    }
    fn source(&self) -> SourceOptions {
        SourceOptions { cpk_list: self.one("--cpk-list").map(PathBuf::from), include_mods: self.flags.contains("--include-mods") }
    }
}

fn load(a: &Args) -> Result<Index, String> {
    Index::load(a.out()).map_err(|e| format!("{e}\n(¿falta `vr-index build --game …`?)"))
}

fn run() -> Result<(), String> {
    let a = parse();
    let cmd = a.pos.first().map(String::as_str).unwrap_or("");
    let arg = |i: usize| a.pos.get(i).cloned().ok_or_else(|| USAGE.to_string());
    match cmd {
        "build" => {
            let game = a.one("--game").ok_or("falta --game")?;
            let opts = BuildOptions {
                source: a.source(),
                thumbs: !a.flags.contains("--no-thumbs"),
                thumb_size: a.one("--thumb-size").and_then(|s| s.parse().ok()).unwrap_or(64),
                texts: !a.flags.contains("--no-texts"),
                audio: !a.flags.contains("--no-audio"),
            };
            let out = a.out();
            let t0 = std::time::Instant::now();
            let progress = |step: &str, detail: &str| eprintln!("[{:>6.1}s] {step}: {detail}", t0.elapsed().as_secs_f32());
            let (idx, rebuilt) = if a.flags.contains("--force") {
                (Index::build(game, &out, &opts, &progress).map_err(|e| e.to_string())?, true)
            } else {
                Index::open_or_build(game, &out, &opts, &progress).map_err(|e| e.to_string())?
            };
            let m = idx.meta();
            let size = std::fs::metadata(out.join(vr_index::INDEX_FILE)).map(|m| m.len()).unwrap_or(0);
            let thumbs_size = dir_size(&out.join("thumbs"));
            println!("{}", if rebuilt { "índice construido" } else { "índice al día (no se reconstruye; --force para forzar)" });
            println!("  carpeta: {}", out.display());
            println!("  juego: {} (build Steam {:?}), huella {}", m.game_version.as_deref().unwrap_or("?"), m.steam_build, m.fingerprint);
            println!("  tiempo: {:.1} s   index.vri: {:.1} MB   miniaturas: {} ({:.1} MB)", m.build_ms as f64 / 1000.0, size as f64 / 1e6, m.thumbs, thumbs_size as f64 / 1e6);
            println!("  entidades: {:?}", m.counts);
            println!("  textos: {} tablas", idx.text_files().count());
            if let Some(r) = &m.read {
                println!("  lectura: {} archivos de CPK, {} sueltos, {:.1} MB, {} CPK abiertos, TOC retail {:?} ms, {} redirigidos a retail", r.from_cpk, r.from_loose, r.bytes as f64 / 1e6, r.cpks_opened, r.toc_scan_ms, r.retail_redirects);
            }
            for w in &m.warnings {
                println!("  aviso: {w}");
            }
        }
        "status" => {
            let game = a.one("--game").ok_or("falta --game")?;
            println!("{:?}", Index::freshness(game, a.out(), &a.source()));
        }
        "info" => {
            let m = Index::read_manifest(a.out()).map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&m).unwrap_or_default());
        }
        "search" => {
            let idx = load(&a)?;
            let mut q = Query::new(arg(1)?).lang(a.lang()).limit(a.one("--limit").and_then(|s| s.parse().ok()).unwrap_or(20));
            q.categories = a.cats();
            for f in a.opts.get("--field").into_iter().flatten() {
                if let Some((k, v)) = f.split_once('=') {
                    q = q.field(k, v);
                }
            }
            let t0 = std::time::Instant::now();
            let hits = idx.search(&q);
            if a.flags.contains("--json") {
                println!("{}", serde_json::to_string_pretty(&hits).unwrap_or_default());
            } else {
                for h in &hits {
                    println!("{:>5}  {:<11} {:<22} {}   [{}: {}]", h.score, h.category.as_str(), h.id, h.name, h.matched_lang, h.matched);
                }
                eprintln!("{} resultados en {:.1} ms", hits.len(), t0.elapsed().as_secs_f64() * 1000.0);
            }
        }
        "get" => {
            let idx = load(&a)?;
            let id = arg(1)?;
            let cats = a.cats();
            let found: Vec<&vr_index::Entity> = idx.get(&id).into_iter().filter(|e| cats.is_empty() || cats.contains(&e.category)).collect();
            let found = if found.is_empty() { cats.iter().filter_map(|c| idx.get_in(*c, &id)).collect() } else { found };
            if found.is_empty() {
                return Err(format!("no hay ninguna entidad con id {id}"));
            }
            let out: Vec<serde_json::Value> = found
                .iter()
                .map(|e| {
                    let mut v = serde_json::to_value(e).unwrap_or_default();
                    v["display_name"] = idx.display_name(e, a.lang()).into();
                    if let Some(p) = idx.thumb_path(e) {
                        v["thumb_path"] = p.display().to_string().into();
                    }
                    v
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        "resolve" => {
            let idx = load(&a)?;
            let r = idx.resolve(&arg(1)?, &a.cats(), a.one("--lang").and_then(lang_index));
            println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
        }
        "text" => {
            let idx = load(&a)?;
            let file = arg(1)?;
            let id = vr_index::parse_key_number(&arg(2)?).ok_or("id de texto no válido")?;
            let t = idx.text_table(&file).ok_or_else(|| format!("no hay tabla de texto {file}"))?;
            match t.all(id) {
                Some(v) => {
                    for (l, s) in vr_index::LANGS.iter().zip(v) {
                        println!("{l:>8}: {s}");
                    }
                }
                None => return Err(format!("{file} no tiene el id {id}")),
            }
        }
        "ls" => {
            let src = GameSource::open(a.one("--game").ok_or("falta --game")?, a.source()).map_err(|e| e.to_string())?;
            for it in src.list_prefix(&arg(1)?) {
                println!("{}\t{}\t{}", it.path, it.cpk.as_deref().unwrap_or("(suelto)"), it.size);
            }
        }
        "peek" => {
            let src = GameSource::open(a.one("--game").ok_or("falta --game")?, a.source()).map_err(|e| e.to_string())?;
            let p = arg(1)?;
            let path = match p.strip_suffix('_') {
                Some(p) => {
                    let (dir, stem) = p.rsplit_once('/').ok_or("ruta sin carpeta")?;
                    src.latest(&format!("{dir}/"), stem).ok_or("no existe esa tabla")?
                }
                None => p,
            };
            println!("## {path} {:?}", src.origin(&path));
            let b = src.read(&path).map_err(|e| e.to_string())?;
            print!("{}", vr_index::peek::peek(&b, &a.pos[2..], 4));
        }
        _ => return Err(USAGE.to_string()),
    }
    Ok(())
}

fn dir_size(p: &std::path::Path) -> u64 {
    let mut n = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push(e.path()),
                    Ok(_) => n += e.metadata().map(|m| m.len()).unwrap_or(0),
                    Err(_) => {}
                }
            }
        }
    }
    n
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
