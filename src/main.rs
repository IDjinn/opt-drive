mod wildcard;
mod tests;
use std::io::Write;
use zip::write::FileOptions;
use zip::ZipWriter;
use std::io;
use std::fs;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::{File, Metadata, ReadDir};
use std::hash::{Hash, Hasher};
use std::iter::{Zip, zip};
use std::path::{Path, PathBuf};
use std::ptr::write_bytes;
use std::time::Instant;
use glob::{glob, MatchOptions};
use rayon::iter::ParallelBridge;
use rayon::prelude::*;
use zip::CompressionMethod::Deflated;
use serde::{Deserialize, Serialize};
use serde::de::Unexpected::Option;
use crate::wildcard::is_folder_path_regex_match;

fn path_hash(path: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug)]
enum MetaData {
    File(FileMetadata),
    Directory(DirectoryMetadata)
}

#[derive(Debug, Serialize, Deserialize)]
struct DirectoryMetadata {
    hash: u64,
    path: String,
    parent_hash: u64,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileMetadata {
    hash: u64,
    directory_hash: u64,
    file_name: String,
    file_extension_hash: u64,
}

struct Settings {
    exclude_extensions: Vec<&'static str>,
    include_extensions: Vec<&'static str>,

    exclude_paths: Vec<&'static str>,
    include_paths: Vec<&'static str>,

    multi_thread_enabled: bool,
    depth: i32
}

const glob_options: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: true,
};

fn main() {
    let settings = Settings {
        exclude_extensions: vec![],
        include_extensions: vec![],

        exclude_paths: vec![".git",
                            ".git.*",
                            "node_modules",
                            ".expo",
                            ".expo-shared",
                            ".fleet",
                            ".vscode",
                            ".next",
                            ".idea",
                            ".gradle",
                            "build",
                            "__tests__",
            ".*.json",
                            ".*.lock",
                            ".*.toml",
        ],
        include_paths: vec!["C:\\dev\\react"],

        multi_thread_enabled: false,
        depth: 4
    };

    let start_time = Instant::now();
    let mut dict: HashMap<u64, MetaData> = HashMap::new();
    let files_tree = if settings.multi_thread_enabled {
        let mut result:Vec<MetaData> = Vec::new();
        for include_path in &settings.include_paths {
            result.extend(read_recursive_parallel(include_path, &settings,settings.depth, 0)) ;
        }
        result
    } else  {
        let mut result:Vec<MetaData> = Vec::new();
        for include_path in &settings.include_paths {
            result.extend(read_recursive(include_path, &settings,settings.depth, 0));
        }
        result
    };
    for fileTree in files_tree {
        if let MetaData::Directory(directory) = fileTree{
            dict.insert(directory.hash, MetaData::Directory(directory));
            continue;
        }

        if let MetaData::File(file) = fileTree{
            dict.insert(file.hash, MetaData::File(file));
            continue;
        }
    }


    let end_time = Instant::now();
    let elapsed_time = end_time - start_time;
    println!("Time spent: {:?}", elapsed_time);
    println!("total of {} paths", dict.len());
    return;
}


fn file_metadata(path: &Path) -> std::option::Option<std::io::Result<Metadata>> {
    let file = File::open(path);
    if file.is_ok() {
        return Some(file.unwrap().metadata());
    }

    return None;
}

//  fn save_file_tree(tree: &HashMap<u64, FileTree>){
//     let path = Path::new("test.zip");
//     let file = File::create(path).unwrap();
//     let mut zip = ZipWriter::new(file);
//     let options = FileOptions::default().compression_level(9).compression_method(Deflated);
//
//     for (key, value) in tree {
//         zip.add_directory(key.to_string(), value.serialize().unwrap(), options);
//     }
//
//     zip.add_directory("test/", Default::default());
// }

fn read_recursive(path: &str, settings: &Settings, mut depth: i32, parent_hash: u64) -> Vec<MetaData> {
    depth -= 1;
    if depth <= 0 {
        return Vec::new();
    }

    let paths = match fs::read_dir(path) {
        Ok(entries) => filter(settings,entries),
        Err(_) => return Vec::new(),
    };

    let mut result: Vec<MetaData> = Vec::new();
    for entry in paths {
        let entry_path = Path::new(&entry);
        if entry_path.is_dir() {
            let directory_path = entry_path.to_string_lossy().to_string();
            let directory_path_hash = path_hash(&directory_path);

            result.push(MetaData::Directory(DirectoryMetadata {
                hash: directory_path_hash,
                path: directory_path.clone(),
                parent_hash,
            }));

            let sub_paths = read_recursive(&directory_path, &settings, depth, directory_path_hash);
            result.extend(sub_paths);
        }
        else{
            let file = entry_path;
            let file_path = file.to_string_lossy().to_string();
            let file_name_hash = path_hash(&file_path);
            let file_extension_hash = path_hash(file_path.split(".").last().unwrap_or_default());

            let directory_path = file.parent().unwrap().to_string_lossy().to_string();
            let directory_path_hash = path_hash(&directory_path);

            result.push(MetaData::File(FileMetadata {
                hash: file_name_hash,
                file_name: file.file_name().unwrap().to_string_lossy().to_string(),
                file_extension_hash,
                directory_hash: directory_path_hash,
            }));

            let sub_paths = read_recursive(&file_path, &settings, depth, file_name_hash);
            result.extend(sub_paths);
        }
    }

    result
}

fn read_recursive_parallel(path: &str, settings: &Settings, mut depth: i32, parent_hash: u64) -> Vec<MetaData> {
    if depth <= 0 {
        return Vec::new();
    }

    depth -= 1;
    let paths = match fs::read_dir(path) {
        Ok(entries) => filter_parallel(settings, entries),
        Err(_) => return Vec::new(),
    };

    let result: Vec<MetaData> = paths
        .par_iter()
        .filter_map(|entry| {
            let entry_path = Path::new(&entry);
            if entry_path.is_dir() {
                let directory_path = entry_path.to_string_lossy().to_string();
                let directory_path_hash = path_hash(&directory_path);

                let mut sub_paths = read_recursive(&directory_path, &settings, depth, directory_path_hash);
                sub_paths.push(MetaData::Directory(DirectoryMetadata {
                    hash: directory_path_hash,
                    path: directory_path.clone(),
                    parent_hash,
                }));

                return Some(sub_paths)
            }
            else{
                let file = entry_path;
                let file_path = file.to_string_lossy().to_string();
                let file_name_hash = path_hash(&file_path);
                let file_extension_hash = path_hash(file_path.split(".").last().unwrap_or_default());

                let directory_path = file.parent().unwrap().to_string_lossy().to_string();
                let directory_path_hash = path_hash(&directory_path);

                let mut sub_paths = read_recursive(&file_path, &settings, depth, file_name_hash);
                sub_paths.push(MetaData::File(FileMetadata {
                    hash: file_name_hash,
                    file_name: file.file_name().unwrap().to_string_lossy().to_string(),
                    file_extension_hash,
                    directory_hash: directory_path_hash,
                }));

                return Some(sub_paths)
            }
        })
        .flatten()
        .collect();

    result
}
fn filter(settings: &Settings, entries: ReadDir) -> Vec<PathBuf> {
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| is_path_valid(&entry.path(), settings))
        .map(|entry| entry.path())
        .collect()
}

fn filter_parallel(settings: &Settings, entries: ReadDir) -> Vec<PathBuf> {
    entries
        .par_bridge()
        .filter_map(|entry| entry.ok())
        .filter(|entry| is_path_valid(&entry.path(), settings))
        .map(|entry| entry.path())
        .collect()
}



fn is_path_valid(path: &Path, settings: &Settings) -> bool {
    let path_str = path.to_string_lossy().to_string().to_lowercase();
    if path.is_file() {
        let extension = path_str.split(".").last().unwrap_or_default();
        let has_filter_for_extensions = !settings.include_extensions.is_empty() || !settings.exclude_extensions.is_empty();
        if has_filter_for_extensions && (!settings.include_extensions.contains(&extension) || settings.exclude_extensions.contains(&extension)) {
            println!("[skipped-ext] [exclude] path = {}", path_str);
            return false;
        }

        return true;
    } else {
        if settings.include_paths.iter().any(|include_path| glob(include_path).ok().unwrap().any(|entry| entry.unwrap() == path)) {
            println!("[path-included] path = {}", path_str);
            return true;
        }

        if settings.exclude_paths.iter().any(|exclude_path| path_str.ends_with(exclude_path)) {
            println!("[path-excluded] path = {}", path_str);
            return false;
        }
    }

    for regular_expression in &settings.exclude_paths {
        if is_folder_path_regex_match(path.to_path_buf(), regular_expression) {
            println!("[excluded-regex] = {}", path_str);
            return false;
        }
    }

        for regular_expression in &settings.include_paths {
        if is_folder_path_regex_match(path.to_path_buf(), regular_expression){
            println!("[included-regex] = {}", path_str);
            return  true;
        }
    }


    println!("[skipped] = {}", path_str);
    false
}