use crate::models::{DiskUsage, Subvolume, SubvolumeId, SubvolumeKind};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("missing field `{0}`")]
    MissingField(&'static str),
    #[error("invalid field `{field}`: {value}")]
    InvalidField { field: &'static str, value: String },
}

pub fn parse_findmnt_pairs(input: &str) -> BTreeMap<String, String> {
    input
        .split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_string(), unquote(value)))
        .collect()
}

pub fn parse_btrfs_subvolume_list(input: &str) -> Result<Vec<Subvolume>, ParseError> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_subvolume_line)
        .collect()
}

fn parse_subvolume_line(line: &str) -> Result<Subvolume, ParseError> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let id_pos = tokens
        .iter()
        .position(|token| *token == "ID")
        .ok_or(ParseError::MissingField("ID"))?;
    let path_pos = tokens
        .iter()
        .position(|token| *token == "path")
        .ok_or(ParseError::MissingField("path"))?;
    let id_value = tokens
        .get(id_pos + 1)
        .ok_or(ParseError::MissingField("ID value"))?;
    let id = id_value
        .parse::<u64>()
        .map_err(|_| ParseError::InvalidField {
            field: "ID",
            value: (*id_value).to_string(),
        })?;
    let path = tokens[path_pos + 1..].join(" ");

    Ok(Subvolume {
        id: SubvolumeId(id),
        uuid: parse_optional_uuid_after(&tokens, "uuid")?,
        parent_uuid: parse_optional_uuid_after(&tokens, "parent_uuid")?,
        path: PathBuf::from(path),
        kind: SubvolumeKind::Normal,
        mountpoint: None,
        readonly: false,
        managed: false,
        unlocked: false,
        tags: Vec::new(),
        created_at: None,
    })
}

fn parse_optional_uuid_after(
    tokens: &[&str],
    key: &'static str,
) -> Result<Option<Uuid>, ParseError> {
    let Some(pos) = tokens.iter().position(|token| *token == key) else {
        return Ok(None);
    };
    let Some(value) = tokens.get(pos + 1) else {
        return Err(ParseError::MissingField(key));
    };
    if *value == "-" {
        return Ok(None);
    }
    Uuid::parse_str(value)
        .map(Some)
        .map_err(|_| ParseError::InvalidField {
            field: key,
            value: (*value).to_string(),
        })
}

/// Parses `btrfs filesystem du -s --raw <path>` output: a header line
/// ("Total   Exclusive  Set shared  Filename") followed by one summary line
/// per requested path. We only ever request a single path, so the first line
/// whose first three tokens are byte counts is the answer.
pub fn parse_btrfs_filesystem_du(input: &str) -> Result<DiskUsage, ParseError> {
    for line in input.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 3 {
            continue;
        }
        let (Ok(total_bytes), Ok(exclusive_bytes), Ok(shared_bytes)) = (
            tokens[0].parse::<u64>(),
            tokens[1].parse::<u64>(),
            tokens[2].parse::<u64>(),
        ) else {
            continue;
        };
        return Ok(DiskUsage {
            total_bytes,
            exclusive_bytes,
            shared_bytes,
        });
    }
    Err(ParseError::MissingField("btrfs filesystem du output"))
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|stripped| stripped.strip_suffix('"'))
        .unwrap_or(value)
        .replace("\\x20", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_findmnt_pairs() {
        let pairs =
            parse_findmnt_pairs(r#"TARGET="/mnt/root" FSTYPE="btrfs" OPTIONS="rw,subvol=@""#);
        assert_eq!(pairs.get("TARGET").unwrap(), "/mnt/root");
        assert_eq!(pairs.get("FSTYPE").unwrap(), "btrfs");
    }

    #[test]
    fn parses_filesystem_du_output() {
        let input = "     Total   Exclusive  Set shared  Filename\n  10485760     1048576     9437184  /mnt/btrfs/@snapshots/home-20260825-143207\n";
        let usage = parse_btrfs_filesystem_du(input).unwrap();
        assert_eq!(usage.total_bytes, 10_485_760);
        assert_eq!(usage.exclusive_bytes, 1_048_576);
        assert_eq!(usage.shared_bytes, 9_437_184);
    }

    #[test]
    fn rejects_filesystem_du_output_without_a_data_line() {
        let input = "     Total   Exclusive  Set shared  Filename\n";
        assert!(parse_btrfs_filesystem_du(input).is_err());
    }

    #[test]
    fn parses_subvolume_list_lines() {
        let input = "ID 256 gen 891 top level 5 uuid 550e8400-e29b-41d4-a716-446655440000 parent_uuid - path @\nID 257 gen 92 top level 5 path @home";
        let parsed = parse_btrfs_subvolume_list(input).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, SubvolumeId(256));
        assert_eq!(parsed[0].path, PathBuf::from("@"));
        assert_eq!(parsed[1].path, PathBuf::from("@home"));
    }
}
