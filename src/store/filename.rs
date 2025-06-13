use std::ffi::OsStr;
use std::path::Path;
use crate::{Error, Result};
use crate::storage::{do_write_string_to_file, File, Storage};

#[derive(Debug, PartialEq, Eq)]
pub enum FileType {
    /// Active data file that is currently being written to
    ActiveData,
    /// Immutable data files that are no longer being written to
    Data,
    /// Hint files containing index information for faster startup
    Hint,
    /// Lock file to ensure only one process can access the database
    Lock,
    /// Merge operation marker file
    Merge,
    /// Temporary files created during operations
    Temp,
    /// Current active file pointer
    Current,
    Log
}

/// Generate filename based on directory, file type and sequence number
pub fn generate_filename(dirname: &str, filetype: FileType, seq: u64) -> String {
    let dirname = Path::new(dirname);
    let filename = match filetype {
        FileType::Lock => "LOCK",
        FileType::Log => "LOG",
        FileType::Current => "CURRENT",
        FileType::Temp => &format!("{:06}.tmp", seq),
        FileType::ActiveData => &format!("{:06}.data", seq),
        FileType::Data => &format!("{:06}.data", seq),
        FileType::Hint => &format!("{:06}.hint", seq),
        FileType::Merge => &format!("merge.{:06}", seq),
    };

    dirname.join(filename)
        .to_string_lossy()
        .replace('\\', "/")  // 强制转换为正斜杠
}

/// Parse filename and return tuple containing file type and sequence number
/// The `filename` should be a valid path.
pub fn parse_filename<P: AsRef<Path>>(filename: P) -> Option<(FileType, u64)> {
    let invalid = "invalid";
    let path = filename.as_ref();
    let file_name = path.file_name().unwrap_or_else(|| OsStr::new(invalid));
    match file_name.to_str() {
        Some("CURRENT") => Some((FileType::Current, 0)),
        Some("LOCK") => Some((FileType::Lock, 0)),
        Some(name) => {
            // Handle merge files (merge.XXXXXX)
            if let Some(suffix) = name.strip_prefix("merge.") {
                return suffix.parse::<u64>()
                    .ok()
                    .map(|seq| (FileType::Merge, seq));
            }

            // Handle files with extensions (XXXXXX.ext)
            if let Some((stem, extension)) = name.rsplit_once('.') {
                let seq = stem.parse::<u64>().ok()?;
                let file_type = match extension {
                    "data" => FileType::Data,
                    "hint" => FileType::Hint,
                    "tmp" => FileType::Temp,
                    _ => return None,
                };
                return Some((file_type, seq));
            }

            // Handle pure numbered files (XXXXXX)
            name.parse::<u64>()
                .ok()
                .map(|seq| (FileType::Data, seq))
        }
        _ => None,
    }
}

/// Update the current active data file pointer in a Bitcask database
pub fn update_current<S: Storage>(env: &S, dir: &str, active_file_num: u64) -> Result<()> {
    // Generate the active data file name (without directory path)
    let active_filename = format!("{:06}.data", active_file_num);

    // Generate temporary file path
    let tmp_path = generate_filename(dir, FileType::Temp, active_file_num);

    // Write the active filename to the temporary file
    let result = do_write_string_to_file(env, active_filename, &tmp_path, true);

    // Atomically rename temp file to CURRENT, or clean up on failure
    match &result {
        Ok(()) => {
            let current_path = generate_filename(dir, FileType::Current, 0);
            env.rename(&tmp_path, &current_path)?;
        }
        Err(_) => {
            env.remove(&tmp_path)?;
        }
    }

    result
}

/// Read the current active file number from CURRENT file
pub fn read_current<S: Storage>(env: &S, dir: &str) -> Result<u64> {
    let current_path = generate_filename(dir, FileType::Current, 0);
    if !env.exists(&current_path) {
        return Err(Error::Corruption("CURRENT file does not exist".to_string()));
    }
    // 定义buffer
    let mut read_buf = vec![];
    
    let current_path = Path::new(&current_path);
    
    // buffer
     
    
    env.open(current_path)?.read(&mut read_buf);
    let content = String::from_utf8(read_buf).unwrap();
    let filename = content.trim();

    // Parse the active file number from filename (e.g., "000001.data" -> 1)
    if let Some(dot_pos) = filename.find('.') {
        let number_part = &filename[..dot_pos];
        number_part.parse::<u64>()
            .map_err(|_| Error::Corruption("Invalid active file number format".to_string()))
    } else {
        Err(Error::Corruption("Invalid active filename format".to_string()))
    }
}

/// Get all data file numbers in the directory, sorted in ascending order
pub fn get_data_file_numbers<S: Storage>(env: &S, dir: &str) -> Result<Vec<u64>> {
    let mut file_numbers = Vec::new();

    for entry in env.list(dir)? {
        if let Some((file_type, seq)) = parse_filename(&entry) {
            if matches!(file_type, FileType::Data | FileType::ActiveData) {
                file_numbers.push(seq);
            }
        }
    }

    file_numbers.sort();
    Ok(file_numbers)
}

/// Check if a merge operation is in progress
pub fn is_merge_in_progress<S: Storage>(env: &S, dir: &str) -> bool {
    for entry in env.list(dir).unwrap_or_default() {
        if let Some((file_type, _)) = parse_filename(&entry) {
            if matches!(file_type, FileType::Merge) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_filename() {
        assert_eq!(
            generate_filename("/tmp", FileType::Data, 1),
            "/tmp/000001.data"
        );
        assert_eq!(
            generate_filename("/tmp", FileType::Hint, 123),
            "/tmp/000123.hint"
        );
        assert_eq!(
            generate_filename("/tmp", FileType::Lock, 0),
            "/tmp/LOCK"
        );
    }

    #[test]
    fn test_parse_filename() {
        assert_eq!(
            parse_filename("000001.data"),
            Some((FileType::Data, 1))
        );
        assert_eq!(
            parse_filename("000123.hint"),
            Some((FileType::Hint, 123))
        );
        assert_eq!(
            parse_filename("LOCK"),
            Some((FileType::Lock, 0))
        );
        assert_eq!(
            parse_filename("merge.000001"),
            Some((FileType::Merge, 1))
        );
        assert_eq!(parse_filename("invalid"), None);
    }
}