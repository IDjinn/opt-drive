#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use crate::wildcard::is_folder_path_regex_match;

    #[test]
    fn up_folder_regex_match_test() {
        const PATH: &str = "C://a//b//c";
        const REGEX: &str = "../b";

        let result = is_folder_path_regex_match(Path::new(PATH).to_path_buf(), REGEX);
        assert_eq!(result, true)
    }

    #[test]
    fn false_up_folder_regex_match_test() {
        const PATH: &str = "C://a//b//c";
        const REGEX: &str = "../x";

        let result = is_folder_path_regex_match(Path::new(PATH).to_path_buf(), REGEX);
        assert_eq!(result, false)
    }

    #[test]
    fn local_folder_path_regex_test() {
        const PATH: &str = "C://a//b//c";
        const REGEX: &str = "./c";

        let result = is_folder_path_regex_match(Path::new(PATH).to_path_buf(), REGEX);
        assert_eq!(result, true)
    }

    #[test]
    fn false_local_folder_path_regex_test() {
        const PATH: &str = "C://a//b//c";
        const REGEX: &str = "./y";

        let result = is_folder_path_regex_match(Path::new(PATH).to_path_buf(), REGEX);
        assert_eq!(result, false)
    }
}