use roxmltree::Node;

/// Human-readable information about a RACF document.
///
/// Every field is optional so a document can carry as much or as little
/// metadata as it needs. The player maps these fields onto MPRIS properties.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.artist.is_none()
            && self.album.is_none()
    }
}

/// Reads the optional `<metadata>` element.
///
/// Fields can be written as child elements or as attributes on `<metadata>`:
///
/// ```xml
/// <metadata>
///     <title>Night Drive</title>
///     <description>A short synth sketch.</description>
///     <artist>Andres</artist>
///     <album>Sketches</album>
/// </metadata>
/// ```
pub(crate) fn parse_metadata(root: Node<'_, '_>) -> Metadata {
    let Some(node) = root.children().find(|node| node.has_tag_name("metadata")) else {
        return Metadata::default();
    };

    Metadata {
        title: field(node, "title"),
        description: field(node, "description"),
        artist: field(node, "artist"),
        album: field(node, "album"),
    }
}

fn field(node: Node<'_, '_>, name: &str) -> Option<String> {
    node.children()
        .find(|child| child.has_tag_name(name))
        .and_then(|child| child.text())
        .or_else(|| node.attribute(name))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use crate::document::parse_document;

    #[test]
    fn parses_metadata_children_and_attributes() {
        let document = parse_document(
            r#"
            <racf>
                <metadata>
                    <title>  Night Drive  </title>
                    <description>A short synth sketch.</description>
                </metadata>
            </racf>
            "#,
        )
        .unwrap();

        assert_eq!(document.metadata.title.as_deref(), Some("Night Drive"));
        assert_eq!(
            document.metadata.description.as_deref(),
            Some("A short synth sketch.")
        );
        assert_eq!(document.metadata.artist, None);
        assert_eq!(document.metadata.album, None);
    }

    #[test]
    fn parses_metadata_attributes_as_fallbacks() {
        let document = parse_document(
            r#"<racf><metadata title="Title" artist="Artist" album="Album" /></racf>"#,
        )
        .unwrap();

        assert_eq!(document.metadata.title.as_deref(), Some("Title"));
        assert_eq!(document.metadata.artist.as_deref(), Some("Artist"));
        assert_eq!(document.metadata.album.as_deref(), Some("Album"));
    }

    #[test]
    fn missing_metadata_is_empty() {
        let document = parse_document(r#"<racf />"#).unwrap();
        assert!(document.metadata.is_empty());
    }
}
