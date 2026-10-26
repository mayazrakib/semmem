use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::models::{Basis, MemoryRecord, MemoryReference, MetadataPatch, UpdateMemory};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    #[serde(deserialize_with = "deserialize_strict")]
    id: i64,
    #[serde(deserialize_with = "deserialize_strict")]
    key: String,
    #[serde(deserialize_with = "deserialize_strict")]
    title: String,
    #[serde(deserialize_with = "deserialize_strict")]
    summary: Option<String>,
    #[serde(deserialize_with = "deserialize_strict")]
    basis: Option<Basis>,
    #[serde(deserialize_with = "deserialize_strict")]
    scope: Option<String>,
    #[serde(deserialize_with = "deserialize_strict")]
    observed_at: Option<i64>,
    #[serde(deserialize_with = "deserialize_strict")]
    verified_at: Option<i64>,
    #[serde(deserialize_with = "deserialize_strict")]
    review_after: Option<i64>,
    #[serde(deserialize_with = "deserialize_strict")]
    r#type: String,
    #[serde(deserialize_with = "deserialize_strict")]
    tags: Vec<String>,
    #[serde(deserialize_with = "deserialize_strict")]
    references: Vec<MemoryReference>,
    #[serde(deserialize_with = "deserialize_strict")]
    system_created_by: String,
    #[serde(deserialize_with = "deserialize_strict")]
    system_updated_by: String,
    #[serde(deserialize_with = "deserialize_strict")]
    system_token: String,
    #[serde(deserialize_with = "deserialize_strict")]
    system_created_at: i64,
    #[serde(deserialize_with = "deserialize_strict")]
    system_updated_at: i64,
}

fn deserialize_strict<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_yaml_ng::Value::deserialize(deserializer)?;

    if has_nonfinite_numbers(&value) {
        return Err(serde::de::Error::custom("YAML numbers must be finite."));
    }

    let value = serde_json::to_value(value).map_err(serde::de::Error::custom)?;

    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

fn has_nonfinite_numbers(value: &serde_yaml_ng::Value) -> bool {
    match value {
        serde_yaml_ng::Value::Number(number) => !number.is_finite(),
        serde_yaml_ng::Value::Sequence(values) => values.iter().any(has_nonfinite_numbers),
        serde_yaml_ng::Value::Mapping(values) => values
            .iter()
            .any(|(key, value)| has_nonfinite_numbers(key) || has_nonfinite_numbers(value)),
        serde_yaml_ng::Value::Tagged(value) => has_nonfinite_numbers(&value.value),
        _ => false,
    }
}

pub fn encode_memory(memory: &MemoryRecord) -> Result<String> {
    let fields = Frontmatter {
        id: memory.id,
        key: memory.key.clone(),
        title: memory.title.clone(),
        summary: memory.metadata.summary.clone(),
        basis: memory.metadata.basis,
        scope: memory.metadata.scope.clone(),
        observed_at: memory.metadata.observed_at,
        verified_at: memory.metadata.verified_at,
        review_after: memory.metadata.review_after,
        r#type: memory.r#type.clone(),
        tags: memory.tags.clone(),
        references: memory.references.clone(),
        system_created_by: memory.system_created_by.clone(),
        system_updated_by: memory.system_updated_by.clone(),
        system_token: memory.system_token.clone(),
        system_created_at: memory.system_created_at,
        system_updated_at: memory.system_updated_at,
    };
    let yaml = serde_yaml_ng::to_string(&fields).context("Could not encode memory frontmatter.")?;

    Ok(format!("---\n{yaml}---\n{}", memory.body))
}

pub fn decode_memory(
    document: &str,
    original: &MemoryRecord,
) -> Result<UpdateMemory> {
    let (frontmatter, body) = split_document(document)?;
    let fields: Frontmatter = serde_yaml_ng::from_str(frontmatter).context(
        "The memory frontmatter is invalid. Preserve every field and use null for unknown values.",
    )?;
    ensure!(
        fields.id == original.id,
        "The frontmatter field 'id' is read-only."
    );
    ensure!(
        fields.key == original.key,
        "The frontmatter field 'key' is read-only."
    );
    ensure!(
        fields.system_token == original.system_token,
        "The frontmatter field 'system_token' is read-only."
    );
    ensure!(
        fields.system_created_at == original.system_created_at,
        "The frontmatter field 'system_created_at' is read-only."
    );
    ensure!(
        fields.system_updated_at == original.system_updated_at,
        "The frontmatter field 'system_updated_at' is read-only."
    );

    ensure!(
        fields.system_created_by == original.system_created_by,
        "The frontmatter field 'system_created_by' is read-only."
    );

    ensure!(
        fields.system_updated_by == original.system_updated_by,
        "The frontmatter field 'system_updated_by' is read-only."
    );


    Ok(UpdateMemory {
        key: original.key.clone(),
        expected_token: original.system_token.clone(),
        title: Some(fields.title),
        body: Some(body.to_owned()),
        metadata: MetadataPatch {
            summary: Some(fields.summary),
            basis: Some(fields.basis),
            scope: Some(fields.scope),
            observed_at: Some(fields.observed_at),
            verified_at: Some(fields.verified_at),
            review_after: Some(fields.review_after),
        },
        r#type: Some(fields.r#type),
        tags: Some(fields.tags),
        references: Some(fields.references),
    })
}

fn split_document(document: &str) -> Result<(&str, &str)> {
    let Some(opening_end) = document.find('\n') else {
        anyhow::bail!("Memory documents must begin with a YAML frontmatter delimiter.");
    };
    let opening = document[..opening_end]
        .strip_suffix('\r')
        .unwrap_or(&document[..opening_end]);
    ensure!(
        opening == "---",
        "Memory documents must begin with a YAML frontmatter delimiter."
    );
    let start = opening_end + 1;
    let mut offset = start;

    for line in document[start..].split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);

        if content == "---" {
            return Ok((&document[start..offset], &document[offset + line.len()..]));
        }

        offset += line.len();
    }

    anyhow::bail!("Memory frontmatter is missing its closing YAML delimiter.")
}

#[cfg(test)]
mod tests {
    use super::{decode_memory, encode_memory};
    use crate::models::{Basis, MemoryMetadata, MemoryRecord, MemoryReference};

    pub(super) fn memory() -> MemoryRecord {
        MemoryRecord {
            id: 7,
            key: "decision".into(),
            title: "Unicode decision".into(),
            body: "\n# Decision café 東京 🦀\r\n\r\nLiteral --- content.\n\n".into(),
            metadata: MemoryMetadata {
                summary: Some("Original summary".into()),
                basis: Some(Basis::STATED),
                scope: Some("project/environment".into()),
                observed_at: Some(10),
                verified_at: None,
                review_after: Some(500),
            },
            r#type: "decision".into(),
            tags: vec!["café".into(), "東京".into()],
            references: vec![MemoryReference {
                target_key: "related".into(),
                reference_type: "related to".into(),
            }],
                system_created_by: "human".into(),
            system_updated_by: "human".into(),
                system_token: "opaque-token".into(),
            system_created_at: 100,
            system_updated_at: 200,
            }
    }

    #[test]
    fn round_trips_all_fields_and_exact_unicode_markdown() {
        let original = memory();
        let document = encode_memory(&original).unwrap();
        let update = decode_memory(&document, &original).unwrap();
        assert_eq!(update.key, original.key);
        assert_eq!(update.expected_token, original.system_token);
        assert_eq!(update.title.as_ref(), Some(&original.title));
        assert_eq!(update.body.as_ref(), Some(&original.body));
        assert_eq!(update.r#type.as_ref(), Some(&original.r#type));
        assert_eq!(update.tags.as_ref(), Some(&original.tags));
        assert_eq!(update.references.as_ref(), Some(&original.references));
        assert_eq!(
            update.metadata.apply(&MemoryMetadata::default()),
            original.metadata
        );
        assert_eq!(update.metadata.verified_at, Some(None));
    }

    #[test]
    fn preserves_body_boundaries_and_accepts_crlf_frontmatter_delimiters() {
        for body in [
            "",
            "# Body",
            "\n\nText\n\n",
            "---\n# Raw delimiter\n",
            " \t\r\nUnicode 東京\r\n",
        ] {
            let mut original = memory();
            original.body = body.into();
            let document = encode_memory(&original).unwrap();
            let boundary = document.find("\n---\n").unwrap() + "\n---\n".len();
            let crlf = format!(
                "{}{}",
                document[..boundary].replace('\n', "\r\n"),
                &document[boundary..]
            );
            assert_eq!(
                decode_memory(&crlf, &original).unwrap().body.as_deref(),
                Some(body)
            );
        }
    }

    #[test]
    fn creates_explicit_null_patches_and_preserves_mutable_text() {
        let original = memory();
        let document = encode_memory(&original)
            .unwrap()
            .replace("summary: Original summary", "summary: null")
            .replace("title: Unicode decision", "title: '  Edited: café 東京  '");
        let update = decode_memory(&document, &original).unwrap();
        assert_eq!(update.metadata.summary, Some(None));
        assert_eq!(update.metadata.scope, Some(original.metadata.scope));
        assert_eq!(update.title.as_deref(), Some("  Edited: café 東京  "));
    }

    #[test]
    fn rejects_changes_to_every_readonly_field() {
        let original = memory();
        let document = encode_memory(&original).unwrap();

        for (before, after) in [
            ("id: 7", "id: 8"),
            ("key: decision", "key: changed"),
            ("system_created_by: human", "system_created_by: robot"),
            ("system_updated_by: human", "system_updated_by: robot"),
            ("system_token: opaque-token", "system_token: another-token"),
            ("system_created_at: 100", "system_created_at: 101"),
            ("system_updated_at: 200", "system_updated_at: 201"),
        ] {
            assert!(document.contains(before));
            let error = decode_memory(&document.replace(before, after), &original).unwrap_err();
            assert!(error.to_string().contains("read-only"));
        }
    }

    #[test]
    fn rejects_missing_unknown_duplicate_malformed_and_invalid_metadata() {
        let original = memory();
        let document = encode_memory(&original).unwrap();

        for field in [
            "summary",
            "basis",
            "scope",
            "observed_at",
            "verified_at",
            "review_after",
            "title",
            "tags",
            "references",
            "system_token",
        ] {
            let marker = format!("{field}:");
            let edited = document
                .lines()
                .filter(|line| !line.starts_with(&marker))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                decode_memory(&edited, &original).is_err(),
                "Missing {field} was accepted"
            );
        }

        for (before, after) in [
            ("title: Unicode decision", "title: 42"),
            ("summary: Original summary", "summary: false"),
            ("verified_at: null", "verified_at: 'yesterday'"),
            ("basis: stated", "basis: unsupported"),
            ("tags:\n- café\n- 東京", "tags: [17]"),
            (
                "target_key: related",
                "target_key: related\n  target_key: duplicate",
            ),
        ] {
            assert!(document.contains(before));
            assert!(decode_memory(&document.replace(before, after), &original).is_err());
        }

        for prefix in [
            "unknown: true\n",
            "id: 7\n",
            "summary: Duplicate\n",
            "broken: [\n",
        ] {
            let edited = document.replacen(
                "---\n",
                &format!("---\n{prefix}"),
                1,
            );
            assert!(decode_memory(&edited, &original).is_err());
        }

        assert!(decode_memory("# Missing frontmatter", &original).is_err());
        assert!(decode_memory("---\nid: 7\n", &original).is_err());
    }

    #[test]
    fn rejects_nonfinite_numbers_without_clearing_nullable_fields() {
        let original = memory();
        let document = encode_memory(&original).unwrap();

        for (field, value) in [
            ("summary", "Original summary"),
            ("basis", "stated"),
            ("scope", "project/environment"),
            ("observed_at", "10"),
            ("verified_at", "null"),
            ("review_after", "500"),
        ] {
            let before = format!("{field}: {value}");
            assert!(document.contains(&before));

            for invalid in [".nan", ".inf", "-.inf"] {
                let edited = document.replace(&before, &format!("{field}: {invalid}"));
                assert!(
                    decode_memory(&edited, &original).is_err(),
                    "Nonfinite {field} was accepted"
                );
            }
        }
    }
}

#[cfg(test)]
mod retired_fields_tests {
    #[test]
    fn rejects_retired_author_frontmatter() {
        let original = super::tests::memory();
        let document = super::encode_memory(&original).unwrap();
        let document = document.replacen("---\n", "---\nauthor: Ada\n", 1);
        assert!(super::decode_memory(&document, &original).is_err());
    }
}
