mod wildcard;
mod tests;

use std::io;
use std::fs;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::{File, ReadDir};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;
use glob::{glob, MatchOptions};
use rayon::iter::ParallelBridge;
use rayon::prelude::*;
use crate::wildcard::is_folder_path_regex_match;

fn path_hash(path: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug)]
struct FileTree {
    hash: u64,
    path: String,
    parent_hash: u64,
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
                            "node_modules",
                            ".expo",
                            ".expo-shared",
                            ".fleet",
                            ".next",
                            ".idea",
                            ".gradle",
                            "build",
                            "__tests__",
            ".*.json"
        ],
        include_paths: vec!["C:\\dev\\react"],

        multi_thread_enabled: false,
        depth: 4
    };

    let start_time = Instant::now();
    let mut dict: HashMap<u64, FileTree> = HashMap::new();
    let files_tree = if settings.multi_thread_enabled {
        let mut result:Vec<FileTree> = Vec::new();
        for include_path in &settings.include_paths {
            result.extend(read_recursive_parallel(include_path, &settings,settings.depth, 0)) ;
        }
        result
    } else  {
        let mut result:Vec<FileTree> = Vec::new();
        for include_path in &settings.include_paths {
            result.extend(read_recursive(include_path, &settings,settings.depth, 0));
        }
        result
    };
    for fileTree in files_tree {
        dict.insert(fileTree.hash, fileTree);
    }
    let end_time = Instant::now();
    let elapsed_time = end_time - start_time;
    println!("Time spent: {:?}", elapsed_time);
    println!("total of {} paths", dict.len());
    return;
}

fn read_recursive(path: &str, settings: &Settings, mut depth: i32, parent_hash: u64) -> Vec<FileTree> {
    depth -= 1;
    if depth <= 0 {
        return Vec::new();
    }

    let paths = match fs::read_dir(path) {
        Ok(entries) => filter(settings,entries),
        Err(_) => return Vec::new(),
    };

    let mut result: Vec<FileTree> = Vec::new();
    for entry in paths {
        let entry_path = Path::new(&entry);
        if entry_path.is_dir() {
            let entry_path_str = entry_path.to_string_lossy().to_string();
            let hash = path_hash(&entry_path_str);
            let tree = FileTree {
                hash,
                path: entry_path_str.clone(),
                parent_hash,
            };
            result.push(tree);
            let sub_paths = read_recursive(&entry_path_str, &settings, depth, hash);
            result.extend(sub_paths);
        }
    }

    result
}

fn read_recursive_parallel(path: &str, settings: &Settings, mut depth: i32, parent_hash: u64) -> Vec<FileTree> {
    if depth <= 0 {
        return Vec::new();
    }

    depth -= 1;
    let paths = match fs::read_dir(path) {
        Ok(entries) => filter_parallel(settings, entries),
        Err(_) => return Vec::new(),
    };

    let result: Vec<FileTree> = paths
        .par_iter()
        .filter_map(|entry| {
            let entry_path = Path::new(entry);
            if entry_path.is_dir() {
                let entry_path_str = entry_path.to_string_lossy().to_string();
                let hash = path_hash(&entry_path_str);
                let tree = FileTree {
                    hash,
                    path: entry_path_str.clone(),
                    parent_hash,
                };
                let sub_paths = read_recursive_parallel(&entry_path_str, settings, depth, hash);
                let mut result_vec = vec![tree];
                result_vec.extend(sub_paths);
                Some(result_vec)
            } else {
                None
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