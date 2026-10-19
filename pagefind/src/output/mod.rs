use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use crate::index::PagefindIndexes;
use crate::{SearchOptions, PAGEFIND_VERSION};
use flate2::write::GzEncoder; // TODO: Replace flate2 with async-compression since we
use flate2::Compression; //   // require that crate for the input compression anyway.
use futures::future::join_all;
use hashbrown::HashMap;
use include_dir::{include_dir, Dir};
use minifier::js::minify;
use rayon::prelude::*;
use tokio::fs::{create_dir_all, File};
use tokio::io::AsyncWriteExt;
use tokio::time::sleep;

mod entry;

const GENERIC_WEB_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/wasm/pagefind_web_bg.unknown.",
    env!("CARGO_PKG_VERSION"),
    ".wasm.gz"
));
const WEB_WASM_FILES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/vendor/wasm");

const WEB_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_web.",
    env!("CARGO_PKG_VERSION"),
    ".js"
));
const WEB_UI_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_ui.",
    env!("CARGO_PKG_VERSION"),
    ".js"
));
const WEB_UI_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_ui.",
    env!("CARGO_PKG_VERSION"),
    ".css"
));
const WEB_MODULAR_UI_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_modular_ui.",
    env!("CARGO_PKG_VERSION"),
    ".js"
));
const WEB_MODULAR_UI_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_modular_ui.",
    env!("CARGO_PKG_VERSION"),
    ".css"
));
const COMPONENT_UI_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_component_ui.",
    env!("CARGO_PKG_VERSION"),
    ".js"
));
const COMPONENT_UI_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_component_ui.",
    env!("CARGO_PKG_VERSION"),
    ".css"
));
const SEARCH_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_public_search_api.",
    env!("CARGO_PKG_VERSION"),
    ".js"
));
const HIGHLIGHT_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_highlight.",
    env!("CARGO_PKG_VERSION"),
    ".js"
));
const WORKER_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/vendor/pagefind_worker.",
    env!("CARGO_PKG_VERSION"),
    ".js"
));

pub struct LanguageMeta {
    pub page_count: usize,
    pub language: String,
    pub hash: String,
    pub wasm: Option<String>,
}

pub async fn write_common_to_disk(
    language_indexes: Vec<LanguageMeta>,
    output_playground: bool,
    outdir: &PathBuf,
    options: &SearchOptions,
) {
    write_common(language_indexes, output_playground, outdir, options, false).await;
}

pub async fn write_common_to_memory(
    language_indexes: Vec<LanguageMeta>,
    output_playground: bool,
    outdir: &PathBuf,
    options: &SearchOptions,
) -> Vec<SyntheticFile> {
    write_common(language_indexes, output_playground, outdir, options, true)
        .await
        .unwrap()
}

async fn write_common(
    language_indexes: Vec<LanguageMeta>,
    output_playground: bool,
    outdir: &PathBuf,
    options: &SearchOptions,
    synthetic: bool,
) -> Option<Vec<SyntheticFile>> {
    let js_version = format!("const pagefind_version = \"{PAGEFIND_VERSION}\";");
    let mut js = vec![];
    minify(&format!("{js_version}\n{WEB_JS}\n{SEARCH_JS}"))
        .write(&mut js)
        .expect("Minifying Pagefind JS failed");

    let mut worker_js = vec![];
    minify(&format!(
        "{js_version}\n{WEB_JS}\n{}",
        String::from_utf8_lossy(WORKER_JS)
    ))
    .write(&mut worker_js)
    .expect("Minifying Pagefind Worker JS failed");

    let entry_meta = entry::PagefindEntryMeta {
        version: PAGEFIND_VERSION,
        languages: HashMap::from_iter(language_indexes.into_iter().map(|i| {
            (
                i.language,
                entry::PagefindEntryLanguage {
                    hash: i.hash,
                    wasm: i.wasm,
                    page_count: i.page_count,
                },
            )
        })),
        include_characters: options.include_characters.clone(),
    };
    let encoded_entry_meta = serde_json::to_string(&entry_meta).unwrap();

    let write_behavior = if synthetic {
        WriteBehavior::Synthetic
    } else {
        WriteBehavior::Disk
    };

    let mut files = vec![
        OutputFile::new(
            outdir.join("pagefind.js"),
            vec![&js],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-highlight.js"),
            vec![HIGHLIGHT_JS],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-worker.js"),
            vec![&worker_js],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-ui.js"),
            vec![WEB_UI_JS],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-ui.css"),
            vec![WEB_UI_CSS],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-modular-ui.js"),
            vec![WEB_MODULAR_UI_JS],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-modular-ui.css"),
            vec![WEB_MODULAR_UI_CSS],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-component-ui.js"),
            vec![COMPONENT_UI_JS],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-component-ui.css"),
            vec![COMPONENT_UI_CSS],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("wasm.unknown.pagefind"),
            vec![GENERIC_WEB_WASM],
            Compress::None,
            write_behavior,
        ),
        OutputFile::new(
            outdir.join("pagefind-entry.json"),
            vec![encoded_entry_meta.as_bytes()],
            Compress::None,
            write_behavior,
        ),
    ];

    if output_playground {
        files.extend([
            OutputFile::new(
                outdir.join("playground/index.html"),
                vec![crate::playground::PLAYGROUND_HTML.as_bytes()],
                Compress::None,
                write_behavior,
            ),
            OutputFile::new(
                outdir.join("playground/pagefind-playground.js"),
                vec![crate::playground::PLAYGROUND_JS.as_bytes()],
                Compress::None,
                write_behavior,
            ),
            OutputFile::new(
                outdir.join("playground/pagefind-playground.css"),
                vec![crate::playground::PLAYGROUND_CSS.as_bytes()],
                Compress::None,
                write_behavior,
            ),
        ]);
    }

    let output_files = write_all(files).await;

    if synthetic {
        Some(output_files)
    } else {
        None
    }
}

impl PagefindIndexes {
    fn lang_wasm_path(&self) -> Option<String> {
        let base_language = self.language.split('-').next().unwrap();
        let wasm_path = format!(
            "pagefind_web_bg.{}.{}.wasm.gz",
            base_language,
            env!("CARGO_PKG_VERSION")
        );

        if WEB_WASM_FILES.contains(&wasm_path) {
            Some(wasm_path)
        } else {
            None
        }
    }

    pub fn get_lang_meta(&self, options: &SearchOptions) -> LanguageMeta {
        let mut wasm_file = None;

        if self.language != "unknown" {
            if self.lang_wasm_path().is_some() {
                wasm_file = Some(self.language.to_string());
            } else {
                options.logger.v_warn(format!(
                    "Note: Pagefind doesn't support stemming for the language {}. \n\
                    Search will still work, but will not match across root words.",
                    self.language
                ));
            }
        }

        LanguageMeta {
            page_count: self.fragments.len(),
            language: self.language.clone(),
            hash: self.meta_index.0.clone(),
            wasm: wasm_file,
        }
    }

    pub async fn write_files_to_disk(&self, options: &SearchOptions, outdir: &PathBuf) {
        self.write_files(options, outdir, false).await;
    }

    pub async fn write_files_to_memory(
        &self,
        options: &SearchOptions,
        outdir: &PathBuf,
    ) -> Vec<SyntheticFile> {
        self.write_files(options, outdir, true).await.unwrap()
    }

    async fn write_files(
        &self,
        options: &SearchOptions,
        outdir: &PathBuf,
        synthetic: bool,
    ) -> Option<Vec<SyntheticFile>> {
        let immutable_write_behaviour = if synthetic {
            WriteBehavior::Synthetic
        } else {
            WriteBehavior::Immutable
        };

        let mut files = vec![OutputFile::new(
            outdir.join(format!("pagefind.{}.pf_meta", &self.meta_index.0)),
            vec![&self.meta_index.1],
            Compress::GZ,
            immutable_write_behaviour,
        )];

        if self.language != "unknown" {
            if let Some(wasm_path) = self.lang_wasm_path() {
                files.push(OutputFile::new(
                    outdir.join(format!("wasm.{}.pagefind", self.language)),
                    vec![WEB_WASM_FILES
                        .get_file(wasm_path)
                        .expect("WASM should exist")
                        .contents()],
                    Compress::None,
                    if synthetic {
                        WriteBehavior::Synthetic
                    } else {
                        WriteBehavior::Disk
                    },
                ));
            } else {
                options.logger.v_warn(format!(
                    "Note: Pagefind doesn't support stemming for the language {}. \n\
                    Search will still work, but will not match across root words.",
                    self.language
                ));
            }
        }

        files.extend(self.fragments.iter().map(|(hash, fragment)| {
            OutputFile::new(
                outdir.join(format!("fragment/{}.pf_fragment", hash)),
                vec![fragment.as_bytes()],
                Compress::GZ,
                immutable_write_behaviour,
            )
        }));

        files.extend(self.word_indexes.iter().map(|(hash, index)| {
            OutputFile::new(
                outdir.join(format!("index/{}.pf_index", hash)),
                vec![index],
                Compress::GZ,
                immutable_write_behaviour,
            )
        }));

        files.extend(self.filter_indexes.iter().map(|(hash, index)| {
            OutputFile::new(
                outdir.join(format!("filter/{}.pf_filter", hash)),
                vec![index],
                Compress::GZ,
                immutable_write_behaviour,
            )
        }));

        let output_files = write_all(files).await;
        if synthetic {
            Some(output_files)
        } else {
            None
        }
    }
}

#[derive(Copy, Clone)]
enum Compress {
    GZ,
    None,
}

#[derive(Copy, Clone)]
enum WriteBehavior {
    Synthetic,
    Immutable,
    Disk,
}

#[derive(Clone)]
pub struct SyntheticFile {
    pub filename: PathBuf,
    pub contents: Vec<u8>,
}

/// A file of the bundle, before it is compressed and written.
struct OutputFile<'a> {
    filename: PathBuf,
    content_chunks: Vec<&'a [u8]>,
    compression: Compress,
    write_behavior: WriteBehavior,
}

/// A file of the bundle with its final contents, ready to be written.
struct EncodedFile<'a> {
    filename: PathBuf,
    contents: Contents<'a>,
    write_behavior: WriteBehavior,
}

enum Contents<'a> {
    Compressed(Vec<u8>),
    Uncompressed(Vec<&'a [u8]>),
}

impl<'a> OutputFile<'a> {
    fn new(
        filename: PathBuf,
        content_chunks: Vec<&'a [u8]>,
        compression: Compress,
        write_behavior: WriteBehavior,
    ) -> Self {
        Self {
            filename,
            content_chunks,
            compression,
            write_behavior,
        }
    }

    /// Compress this file's contents if needed, or return `None` if it shouldn't be written.
    fn encode(self) -> Option<EncodedFile<'a>> {
        // For "immutable" (hashed) files, don't re-write them as the contents _should_ be unchanged.
        if matches!(self.write_behavior, WriteBehavior::Immutable) && self.filename.exists() {
            return None;
        }

        let contents = match self.compression {
            Compress::GZ => {
                let mut gz = GzEncoder::new(Vec::new(), Compression::default());
                for chunk in self.content_chunks {
                    gz.write_all(b"pagefind_dcd").unwrap();
                    gz.write_all(chunk).unwrap();
                }
                Contents::Compressed(gz.finish().ok()?)
            }
            Compress::None => Contents::Uncompressed(self.content_chunks),
        };

        Some(EncodedFile {
            filename: self.filename,
            contents,
            write_behavior: self.write_behavior,
        })
    }
}

/// Compress the files in parallel, then write them, or return them in order if synthetic.
async fn write_all(files: Vec<OutputFile<'_>>) -> Vec<SyntheticFile> {
    let encoded: Vec<_> = files
        .into_par_iter()
        .filter_map(OutputFile::encode)
        .collect();

    join_all(encoded.into_iter().map(write))
        .await
        .into_iter()
        .flatten()
        .collect()
}

async fn write(file: EncodedFile<'_>) -> Option<SyntheticFile> {
    let EncodedFile {
        filename,
        contents,
        write_behavior,
    } = file;

    if matches!(write_behavior, WriteBehavior::Synthetic) {
        let contents = match contents {
            Contents::Compressed(contents) => contents,
            Contents::Uncompressed(chunks) => chunks.into_iter().flatten().cloned().collect(),
        };
        return Some(SyntheticFile { filename, contents });
    }

    if let Some(parent) = filename.parent() {
        create_dir_all(parent).await.unwrap();
    }

    let mut output_file = File::create(&filename).await;
    while output_file.is_err() {
        sleep(Duration::from_millis(100)).await;
        output_file = File::create(&filename).await;
    }
    let mut file = output_file.unwrap();

    match contents {
        Contents::Compressed(contents) => file.write_all(&contents).await.unwrap(),
        Contents::Uncompressed(chunks) => {
            for chunk in chunks {
                file.write_all(chunk).await.unwrap();
            }
        }
    }
    None
}
