use crate::models::{Subvolume, SubvolumeId, SubvolumeKind};
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
    fn parses_subvolume_list_lines() {
        let input = "ID 256 gen 891 top level 5 uuid 550e8400-e29b-41d4-a716-446655440000 parent_uuid - path @\nID 257 gen 92 top level 5 path @home";
        let parsed = parse_btrfs_subvolume_list(input).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, SubvolumeId(256));
        assert_eq!(parsed[0].path, PathBuf::from("@"));
        assert_eq!(parsed[1].path, PathBuf::from("@home"));
    }
}
