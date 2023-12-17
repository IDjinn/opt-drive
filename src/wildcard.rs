use std::path::{Path, PathBuf};
use regex::Regex;
use {
    once_cell::sync::Lazy,
};
static RELATIVE_PATH_REGEX: Lazy<Regex> = Lazy::new(||Regex::new(r"^(\.\/)?(.+)\/$").unwrap());
static UP_FOLDER_PATH_REGEX: Lazy<Regex> = Lazy::new(||Regex::new("../").unwrap());
static FILE_OR_FOLDER_NAME_REGEX: Lazy<Regex> = Lazy::new(||Regex::new(r"[^/\\]+$").unwrap());

pub fn is_folder_path_regex_match(path: PathBuf, regex: &str) -> bool {
    let clean_regex = regex.replace("\\", "/").replace("./", "").to_string();
    let path_str_clean = path.to_string_lossy().replace("\\", "/").to_string();
    let regular_expression = Regex::new(&clean_regex).unwrap();
    if regular_expression.is_match(&path_str_clean) {
        return true;
    }

    let matches_any_folder_or_file = !clean_regex.ends_with("/");
    let up_folders_count = match_up_folder_count(&clean_regex);
    if up_folders_count > 0 {
        if let Some(final_path) = do_up_folders_of_path(path, up_folders_count) {
            if !matches_any_folder_or_file {
                return true;
            }

            let final_path_str =final_path.to_string_lossy().to_string();
            let folder_or_file_name = regex.split("/").last();
            if folder_or_file_name.is_none() { // should not happen at this point.
                return false;
            }

            return final_path_str.ends_with(folder_or_file_name.unwrap());
        }
    }
    false
}


fn do_up_folders_of_path(path: PathBuf, up_folders_count: usize) -> Option<PathBuf> {
    let mut up_parent = Some(path);
    let mut current_folder = 0;

    while current_folder < up_folders_count {
        current_folder += 1;

        if let Some(current) = up_parent.clone() {
            if let Some(parent) = current.parent() {
                up_parent = Some(parent.to_path_buf());
                continue;
            }
        }
            break;
    }

    if up_folders_count == current_folder {
        up_parent
    } else {
        None
    }
}

fn match_up_folder_count(path: &str) -> usize {
    let captures_count = UP_FOLDER_PATH_REGEX.captures_iter(path).count();
    if captures_count == 0 {
        return 0;
    }

    if path.ends_with('/') {
        captures_count
    } else {
        captures_count - 1
    }
}
