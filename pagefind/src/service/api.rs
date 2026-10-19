//! The programmatic service interface for integrating Pagefind into another Rust project.
//!
//! # Examples
//!
//! ```
//! use pagefind::api::{PagefindIndex};
//! use pagefind::options::{PagefindServiceConfig};
//!
//! #[tokio::main]
//! async fn main() {
//!     let options = PagefindServiceConfig::builder()
//!         .keep_index_url(true)
//!         .force_language("en".to_string())
//!         .build();
//!     let mut index = PagefindIndex::new(Some(options)).expect("Options should be valid");
//!     let indexing_response = index
//!         .add_html_file(
//!             Some("test/index.html".into()),
//!             None,
//!             "<html><body><h1>Test content</h1></body></html>".into(),
//!         )
//!         .await;
//!
//!     if let Ok(file) = indexing_response {
//!         println!("Page word count: {}", file.page_word_count);
//!         println!("Page URL: {}", file.page_url);
//!     }
//!
//!     let files_response = index.get_files().await;
//!     if let Ok(files) = files_response {
//!         println!("Have {} files to write to disk", files.len());
//!     }
//! }
//! ```

pub use crate::output::SyntheticFile;
use anyhow::{bail, Result};
use rust_patch::Patch;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use crate::{
    fossick::{parser::DomParserResult, Fossicker},
    options::PagefindServiceConfig,
    PagefindInboundConfig, SearchOptions, SearchState,
};

#[derive(Debug)]
pub struct IndexedFileResponse {
    pub page_word_count: u32,
    pub page_url: String,
    pub page_meta: BTreeMap<String, String>,
}

pub struct PagefindIndex {
    search_index: Arc<tokio::sync::Mutex<SearchState>>,
}

impl PagefindIndex {
    /// Create a new PagefindIndex instance that files can be added to.
    ///
    /// # Arguments
    /// * `config` - An optional PagefindServiceConfig to override default options for the service.
    ///
    /// # Returns
    /// A PagefindIndex instance if search options are valid, otherwise an Error.
    pub fn new(config: Option<PagefindServiceConfig>) -> Result<Self> {
        let mut service_options: PagefindInboundConfig =
            serde_json::from_str("{}").expect("All fields have serde defaults");

        service_options.service = true;
        if let Some(config) = config {
            service_options = config.apply(service_options);
        }

        let options = SearchOptions::load(service_options)?;
        Ok(Self {
            search_index: Arc::new(tokio::sync::Mutex::new(SearchState::new(options))),
        })
    }

    /// Add an HTML file that isn't on disk into this search index.
    /// Either a filepath or a URL must be provided.
    ///
    /// # Arguments
    /// * `source_path` - The source path of the HTML file if it were to exist on disk.
    /// * `url` - An explicit URL to use, instead of having Pagefind compute the URL based on the `source_path`.
    /// * `content` - The source HTML content of the file to be parsed.
    ///
    /// # Returns
    /// Metadata about the added file if successful, otherwise an Error.
    pub async fn add_html_file(
        &mut self,
        source_path: Option<String>,
        url: Option<String>,
        content: String,
    ) -> Result<IndexedFileResponse> {
        if source_path.is_none() && url.is_none() {
            bail!("Either source_path or url must be provided");
        }

        let file = Fossicker::new_synthetic(source_path.map(PathBuf::from), url, content);
        let state = Arc::clone(&self.search_index);
        let data = tokio::task::spawn_blocking(move || {
            state.blocking_lock().fossick_one(file)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {e}"))??;

        Ok(IndexedFileResponse {
            page_word_count: data.fragment.data.word_count as u32,
            page_url: data.fragment.data.url,
            page_meta: data.fragment.data.meta,
        })
    }

    /// Index a custom record that isn't backed by an HTML file.
    ///
    /// # Arguments
    /// * `url` - The output URL of this record. Pagefind will not alter this.
    /// * `content` - The raw content of this record.
    /// * `language` - What language is this record written in. Expects an ISO 639-1 code.
    /// * `meta` - The metadata to attach to this record. Supplying a `title` is highly recommended.
    /// * `filters` - The filters to attach to this record.
    /// * `sort` - The sort keys to attach to this record.
    ///
    /// # Returns
    /// Metadata about the added record if successful, otherwise an Error.
    pub async fn add_custom_record(
        &mut self,
        url: String,
        content: String,
        language: String,
        meta: Option<BTreeMap<String, String>>,
        filters: Option<BTreeMap<String, Vec<String>>>,
        sort: Option<BTreeMap<String, String>>,
    ) -> Result<IndexedFileResponse> {
        let force_language = self
            .search_index
            .lock()
            .await
            .options
            .force_language
            .clone();
        let data = DomParserResult {
            digest: content,
            filters: filters.unwrap_or_default(),
            sort: sort.unwrap_or_default(),
            meta: meta.unwrap_or_default(),
            anchor_content: BTreeMap::new(),
            has_custom_body: false,
            force_inclusion: true,
            has_html_element: true,
            has_old_bundle_reference: false,
            has_default_ui_reference: false,
            language: force_language.unwrap_or(language),
        };
        let file = Fossicker::new_with_data(url, data);
        let state = Arc::clone(&self.search_index);
        let data = tokio::task::spawn_blocking(move || {
            state.blocking_lock().fossick_one(file)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {e}"))??;

        Ok(IndexedFileResponse {
            page_word_count: data.fragment.data.word_count as u32,
            page_url: data.fragment.data.url,
            page_meta: data.fragment.data.meta,
        })
    }

    /// Index a directory of HTML files from disk.
    ///
    /// # Arguments
    /// * `path` - The path to the directory to index.
    /// * `glob` - A glob pattern to match files in the directory. If not provided, the default glob pattern will be used.
    ///
    /// # Returns
    /// The number of pages indexed if successful, otherwise an Error.
    pub async fn add_directory(&mut self, path: String, glob: Option<String>) -> Result<usize> {
        let defaults: PagefindInboundConfig =
            serde_json::from_str("{}").expect("All fields have serde defaults");
        let glob = glob.unwrap_or(defaults.glob);

        let state = Arc::clone(&self.search_index);
        let page_count = tokio::task::spawn_blocking(move || {
            state.blocking_lock().fossick_many(PathBuf::from(path), glob)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task join error: {e}"))??;

        Ok(page_count)
    }

    /// Build the search index for this instance and hold it in memory.
    pub async fn build_indexes(&mut self) -> Result<()> {
        self.search_index.lock().await.build_indexes().await
    }

    /// Build the search index for this instance and write the files to disk.
    ///
    /// # Arguments
    /// * `output_path` - The path to write the files to. If not provided, the default output path will be used.
    ///
    /// # Returns
    /// The path files were written to if successful, otherwise an Error.
    pub async fn write_files(&mut self, output_path: Option<String>) -> Result<String> {
        let mut state = self.search_index.lock().await;
        state.build_indexes().await?;
        let resolved_output_path = state.write_files(output_path.map(Into::into)).await;

        Ok(resolved_output_path.to_string_lossy().into())
    }

    /// Build the search index for this instance and return the files as a list of
    /// SyntheticFileResponse.
    ///
    /// # Returns
    /// A list of SyntheticFiles containing the path and content of each file.
    pub async fn get_files(&mut self) -> Result<Vec<SyntheticFile>> {
        let mut state = self.search_index.lock().await;
        state.build_indexes().await?;
        Ok(state.get_files().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_file() {
        let options = PagefindServiceConfig::builder()
            .keep_index_url(true)
            .force_language("en".to_string())
            .build();
        let mut index = PagefindIndex::new(Some(options)).unwrap();
        let file_response = index
            .add_html_file(
                Some("test/index.html".into()),
                None,
                "<html><body><h1>Test content</h1></body></html>".into(),
            )
            .await;

        let file = file_response.expect("Adding a file should succeed");
        assert_eq!(file.page_word_count, 2);
        assert_eq!(file.page_url, "/test/index.html");

        let files_response = index.get_files().await;

        let files = files_response.expect("Getting files should succeed");
        let filenames: Vec<_> = files.into_iter().map(|f| f.filename).collect();
        assert!(filenames.contains(&PathBuf::from("pagefind.js")));
        assert!(filenames.contains(&PathBuf::from("pagefind-ui.js")));
        assert!(filenames.contains(&PathBuf::from("pagefind-ui.css")));
        assert!(filenames.contains(&PathBuf::from("wasm.en.pagefind")));
        assert!(filenames.contains(&PathBuf::from("pagefind-entry.json")));
        assert!(filenames
            .iter()
            .any(|f| f.to_string_lossy().ends_with(".pf_meta")));
        assert!(filenames
            .iter()
            .any(|f| f.to_string_lossy().ends_with(".pf_fragment")));
        assert!(filenames
            .iter()
            .any(|f| f.to_string_lossy().ends_with(".pf_index")));
    }

    fn test_index() -> PagefindIndex {
        let options = PagefindServiceConfig::builder()
            .keep_index_url(true)
            .force_language("en".to_string())
            .build();
        PagefindIndex::new(Some(options)).unwrap()
    }

    /// A page to add: its source path, its URL, and its HTML.
    type TestPage = (Option<&'static str>, Option<&'static str>, &'static str);

    fn html_file(
        source_path: Option<&'static str>,
        url: Option<&'static str>,
        content: &'static str,
    ) -> TestPage {
        (source_path, url, content)
    }

    async fn add_test_pages(index: &mut PagefindIndex) {
        for (source_path, url, content) in test_pages() {
            index
                .add_html_file(
                    source_path.map(String::from),
                    url.map(String::from),
                    content.to_string(),
                )
                .await
                .unwrap();
        }
    }

    /// Pages with headings, metadata, weights, and filters; `/alpha/` is given twice.
    /// They use one filter value only: the values of a filter are encoded in hash map
    /// order, so two values would make two builds of the same pages differ.
    fn test_pages() -> Vec<TestPage> {
        vec![
            html_file(
                None,
                Some("/alpha/"),
                "<html lang=\"en\"><body><h1>Alpha</h1><p>The first page about apples.</p>\
                 <h2 id=\"orchards\">Orchards</h2><p>Apples grow in orchards.</p></body></html>",
            ),
            html_file(
                None,
                Some("/beta/"),
                "<html lang=\"en\"><body><h1 data-pagefind-meta=\"title\">Beta guide</h1>\
                 <span data-pagefind-meta=\"author:Ann\"></span>\
                 <p data-pagefind-weight=\"5\">Bananas matter most here.</p>\
                 <p data-pagefind-filter=\"kind\">guide</p></body></html>",
            ),
            html_file(
                Some("docs/gamma.html"),
                None,
                "<html lang=\"en\"><body><h1>Gamma</h1><h2 id=\"cherries\">Cherries</h2>\
                 <p data-pagefind-weight=\"0.5\">Cherries are small.</p></body></html>",
            ),
            html_file(
                Some("docs/delta.html"),
                Some("/delta/"),
                "<html lang=\"en\"><body><h1>Delta</h1><p>Dates and figs.</p>\
                 <h3 id=\"figs\">Figs</h3><p>Figs are sweet.</p>\
                 <p data-pagefind-filter=\"kind\">guide</p></body></html>",
            ),
            html_file(
                None,
                Some("/alpha/"),
                "<html lang=\"en\"><body><h1>Alpha again</h1><p>The second page about apricots.</p>\
                 </body></html>",
            ),
            html_file(
                None,
                Some("/epsilon/"),
                "<html lang=\"en\"><body><h1 data-pagefind-meta=\"title\">Epsilon</h1>\
                 <img data-pagefind-meta=\"image[src]\" src=\"/e.png\">\
                 <p>Elderberries, apples, and bananas.</p></body></html>",
            ),
        ]
    }

    fn file_name(path: &std::path::Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    /// The SHA-1 of the name and decoded contents of each gzipped file of a bundle
    /// but the meta file, in name order, and the number of those files.
    /// The meta file is left out because it records the Pagefind version.
    fn decoded_digest(files: &[(String, Vec<u8>)]) -> (String, usize) {
        use sha1::{Digest, Sha1};
        use std::io::Read;

        let mut files: Vec<_> = files
            .iter()
            .filter(|(name, _)| {
                [".pf_fragment", ".pf_index", ".pf_filter"]
                    .iter()
                    .any(|extension| name.ends_with(extension))
            })
            .collect();
        files.sort();

        let mut hasher = Sha1::new();
        for (name, contents) in &files {
            let mut decoded = vec![];
            flate2::read::GzDecoder::new(&contents[..])
                .read_to_end(&mut decoded)
                .unwrap_or_else(|e| panic!("{name} should be gzipped: {e}"));
            assert!(decoded.starts_with(b"pagefind_dcd"), "{name}");
            hasher.update(name.as_bytes());
            hasher.update(&decoded);
        }
        (format!("{:x}", hasher.finalize()), files.len())
    }

    /// The files of the test pages' bundle, as `get_files` returns them.
    async fn bundle_in_memory() -> Vec<(String, Vec<u8>)> {
        let mut index = test_index();
        add_test_pages(&mut index).await;
        index
            .get_files()
            .await
            .unwrap()
            .into_iter()
            .map(|f| (file_name(&f.filename), f.contents))
            .collect()
    }

    /// A new, empty folder for a test to write a bundle to.
    fn empty_folder(test: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("pagefind-{test}-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&folder);
        folder
    }

    /// Write the test pages' bundle into `folder`.
    async fn write_bundle(folder: &std::path::Path) {
        let mut index = test_index();
        add_test_pages(&mut index).await;
        index
            .write_files(Some(folder.to_string_lossy().into_owned()))
            .await
            .unwrap();
    }

    /// Every file under `folder`, by its path relative to `folder`, in name order.
    fn read_folder(folder: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        fn read_tree(
            root: &std::path::Path,
            dir: &std::path::Path,
            files: &mut Vec<(String, Vec<u8>)>,
        ) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    read_tree(root, &path, files);
                } else {
                    let name = file_name(path.strip_prefix(root).unwrap());
                    files.push((name, std::fs::read(&path).unwrap()));
                }
            }
        }

        let mut files = vec![];
        read_tree(folder, folder, &mut files);
        files.sort();
        files
    }

    #[tokio::test]
    async fn the_gzipped_files_of_a_bundle_decode_to_the_same_payloads() {
        // A fragment for each of the five distinct URLs, the word index chunk, and the filter.
        assert_eq!(
            decoded_digest(&bundle_in_memory().await),
            ("7249e6f9441c49231b83a1c3f411ef189c3113be".to_string(), 7)
        );
    }

    #[tokio::test]
    async fn the_files_of_a_bundle_in_memory_come_in_a_fixed_order() {
        let names: Vec<_> = bundle_in_memory()
            .await
            .into_iter()
            .map(|(name, _)| {
                // The meta file's hash depends on the Pagefind version.
                if name.ends_with(".pf_meta") {
                    "pagefind.<hash>.pf_meta".to_string()
                } else {
                    name
                }
            })
            .collect();
        assert_eq!(
            names,
            [
                "pagefind.<hash>.pf_meta",
                "wasm.en.pagefind",
                "fragment/en_e74aa47.pf_fragment",
                "fragment/en_62f4888.pf_fragment",
                "fragment/en_7602e77.pf_fragment",
                "fragment/en_1b7958a.pf_fragment",
                "fragment/en_21505e5.pf_fragment",
                "index/en_3cf6dfe.pf_index",
                "filter/en_80f54ed.pf_filter",
                "pagefind.js",
                "pagefind-highlight.js",
                "pagefind-worker.js",
                "pagefind-ui.js",
                "pagefind-ui.css",
                "pagefind-modular-ui.js",
                "pagefind-modular-ui.css",
                "pagefind-component-ui.js",
                "pagefind-component-ui.css",
                "wasm.unknown.pagefind",
                "pagefind-entry.json",
            ]
        );
    }

    #[tokio::test]
    async fn the_bundle_on_disk_is_the_bundle_in_memory() {
        let folder = empty_folder("bundle-on-disk");
        write_bundle(&folder).await;
        let on_disk = read_folder(&folder);
        std::fs::remove_dir_all(&folder).unwrap();

        let mut in_memory = bundle_in_memory().await;
        in_memory.sort();
        assert_eq!(on_disk, in_memory);
    }

    #[tokio::test]
    async fn writing_a_bundle_again_keeps_its_hashed_files_and_rewrites_the_others() {
        let folder = empty_folder("bundle-written-twice");
        write_bundle(&folder).await;
        let fragment = read_folder(&folder)
            .into_iter()
            .map(|(name, _)| name)
            .find(|name| name.ends_with(".pf_fragment"))
            .unwrap();
        std::fs::write(folder.join(&fragment), b"marker").unwrap();
        std::fs::write(folder.join("pagefind-ui.css"), b"marker").unwrap();

        write_bundle(&folder).await;
        let fragment_contents = std::fs::read(folder.join(&fragment)).unwrap();
        let css_contents = std::fs::read(folder.join("pagefind-ui.css")).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(fragment_contents, b"marker");
        assert_ne!(css_contents, b"marker");
    }
}
