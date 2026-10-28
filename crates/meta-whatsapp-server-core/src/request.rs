//! What the operations' requests share, whatever adapter carried them: a
//! Meta object (a send's `template`, a template definition) read into the
//! library's type without a key of the request silently dropped
//! ([`meta_object`]); a Meta object id checked before it becomes a Graph
//! path segment ([`graph_id`], [`typed_graph_id`]).

use meta_whatsapp_rs::core::error::ValidationError;
use meta_whatsapp_rs::core::ids::is_numeric_id;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ServiceError;

/// A Meta object id from a path, a query or a body (a media id, a
/// template id, a WABA id): the library's numeric id shape (1 to
/// `MAX_NUMERIC_ID_LEN` (64) ASCII digits, `meta_whatsapp_rs::core::ids`),
/// and not starting with `0` (decision D4 compares the exact string, so
/// `0102…` and `102…` must not both pass). Checked before any request: an
/// id is a Graph path segment, and an operation on one kind of object
/// must not reach another (`DELETE /{id}` deletes whatever node the id
/// names).
///
/// # Errors
///
/// `422 invalid_request` on `field`.
pub fn graph_id(field: &'static str, id: &str) -> Result<(), ServiceError> {
    if is_numeric_id(id) && !id.starts_with('0') {
        Ok(())
    } else {
        Err(ServiceError::invalid(field))
    }
}

/// [`graph_id`], then the typed id it names (`TemplateId`, `MediaId`,
/// `WabaId`, …), whose own check [`graph_id`] implies.
///
/// # Errors
///
/// `422 invalid_request` on `field`.
pub fn typed_graph_id<T>(field: &'static str, id: &str) -> Result<T, ServiceError>
where
    T: for<'a> TryFrom<&'a str, Error = ValidationError>,
{
    graph_id(field, id)?;
    T::try_from(id).map_err(|_| ServiceError::invalid(field))
}

/// A value of one of Meta's enums (a quality rating, a template status),
/// as its wire string: what the answers carry, so that a value the
/// library does not name is answered as Meta wrote it.
pub(crate) fn wire<T: Serialize>(value: Option<&T>) -> Option<String> {
    value
        .and_then(|v| serde_json::to_value(v).ok())
        .and_then(|v| v.as_str().map(str::to_owned))
}

/// A Meta object carried by a request (a send's `template`, a template
/// definition), read into the library's type `T`. Request bodies reject
/// unknown fields (docs/design/server.md, section 4.1), and `T` would drop
/// a key it does not know without a word: the object is read, written
/// back, and a key of the request missing from what the library would send
/// (a misspelling, a field Meta documents that the library lacks) is `422
/// invalid_request` on its path under `field` (`template.components[0].x`),
/// never silently left out. A key holding nothing (`null`, `[]`, `{}`) may
/// be left out, and a flat list of values may come back wrapped in a list
/// (the library writes `body_text: ["a", "b"]`, Meta's positional
/// parameters syntax in `templates/components`, as `[["a", "b"]]`): a
/// list of values holds no key. An empty `field` names the request's body
/// (`body`, and a key's path from its root).
///
/// # Errors
///
/// `422 invalid_request` on `field` (not a `T`) or on the dropped key's
/// path; `500 internal` if `T` cannot be written back.
pub fn meta_object<T>(field: &'static str, value: &serde_json::Value) -> Result<T, ServiceError>
where
    T: DeserializeOwned + Serialize,
{
    let object = T::deserialize(value)
        .map_err(|_| ServiceError::invalid(if field.is_empty() { "body" } else { field }))?;
    let sent = serde_json::to_value(&object).map_err(|_| ServiceError::internal())?;
    match dropped_key(value, &sent, field) {
        Some(path) => Err(ServiceError::invalid(path)),
        None => Ok(object),
    }
}

/// The path of the first key of `request` that `sent` lacks (see
/// [`meta_object`]). A key that is not a plain name is reported as its
/// parent's path: the answer never echoes arbitrary input. A list that
/// comes back shorter or longer is reported as a whole (an element could
/// have held the key), except a flat list of values written back as the
/// only element of a list, which is compared with that element.
fn dropped_key(
    request: &serde_json::Value,
    sent: &serde_json::Value,
    path: &str,
) -> Option<String> {
    use serde_json::Value;
    let empty = |v: &Value| match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    };
    let join = |key: &str| {
        let plain = !key.is_empty()
            && key.len() <= 64
            && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        match (path.is_empty(), plain) {
            (_, false) if !path.is_empty() => path.to_owned(),
            (_, false) => "body".to_owned(),
            (true, true) => key.to_owned(),
            (false, true) => format!("{path}.{key}"),
        }
    };
    match (request, sent) {
        (Value::Object(request), Value::Object(sent)) => {
            request.iter().find_map(|(key, value)| match sent.get(key) {
                None if empty(value) => None,
                None => Some(join(key)),
                Some(written) => dropped_key(value, written, &join(key)),
            })
        }
        (Value::Array(items), Value::Array(written)) => {
            // The library's one reshaping: a flat list of values (no
            // object, no list) written back wrapped, `["a", "b"]` as
            // `[["a", "b"]]` (`BodyExample::body_text`). Compared with
            // what it wraps, so a value lost there is still refused.
            if let [inner @ Value::Array(_)] = written.as_slice()
                && items.iter().all(|v| !v.is_array() && !v.is_object())
            {
                return dropped_key(request, inner, path);
            }
            if items.len() != written.len() {
                return Some(if path.is_empty() { "body" } else { path }.to_owned());
            }
            items
                .iter()
                .zip(written)
                .enumerate()
                .find_map(|(i, (value, written))| {
                    dropped_key(value, written, &format!("{path}[{i}]"))
                })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::client::templates::{TemplateDefinition, TemplateMessage};
    use serde_json::json;

    use super::*;

    /// A Meta id is 1 to 64 digits, not starting with `0`. Decisive: each
    /// clause, the length cap included.
    #[test]
    fn a_graph_id_is_up_to_64_digits() {
        use meta_whatsapp_rs::core::ids::TemplateId;
        assert!(graph_id("id", "1").is_ok());
        assert!(graph_id("id", &"9".repeat(64)).is_ok());
        for bad in [
            String::new(),
            "9".repeat(65),
            "0123".to_owned(),
            "12a".to_owned(),
            "1/2".to_owned(),
            "../x".to_owned(),
        ] {
            let refused = graph_id("media_id", &bad).unwrap_err();
            assert_eq!(refused.field(), Some("media_id"), "{bad:?}");
        }
        let id: TemplateId = typed_graph_id("id", "1407680676729941").unwrap();
        assert_eq!(id.as_str(), "1407680676729941");
        let refused = typed_graph_id::<TemplateId>("id", "x1").unwrap_err();
        assert_eq!(refused.field(), Some("id"));
    }

    /// The field a refused Meta object names.
    fn refused<T>(field: &'static str, value: &serde_json::Value) -> Option<String>
    where
        T: DeserializeOwned + Serialize,
    {
        meta_object::<T>(field, value)
            .err()
            .map(|error| error.field().unwrap_or("?").to_owned())
    }

    /// templates/components, "Positional parameters syntax": `body_text`
    /// is a flat list of example values, one per parameter. The library
    /// writes it as `[[..]]`, and that reshaping loses nothing: the
    /// definition is accepted. Decisive: the flat list compared with the
    /// list it is wrapped in, not refused for its length.
    #[test]
    fn metas_flat_body_text_is_accepted() {
        let flat = json!({"name": "order_update", "language": "en_US", "category": "UTILITY",
            "components": [{"type": "body", "text": "Hi {{1}}, order {{2}} shipped.",
                            "example": {"body_text": ["Pablo", "860198"]}}]});
        let definition = meta_object::<TemplateDefinition>("", &flat).unwrap();
        assert_eq!(
            serde_json::to_value(&definition).unwrap()["components"][0]["example"]["body_text"],
            json!([["Pablo", "860198"]])
        );
        // One value, and the nested shape of the reference, too.
        for body_text in [
            json!(["Pablo"]),
            json!([["Pablo", "860198"]]),
            json!("Pablo"),
        ] {
            let mut shape = flat.clone();
            shape["components"][0]["example"]["body_text"] = body_text.clone();
            assert_eq!(
                refused::<TemplateDefinition>("", &shape),
                None,
                "{body_text}"
            );
        }
    }

    /// A list the round trip makes shorter or longer is refused as a
    /// whole: its missing element could have held a key. Only the
    /// wrapping of a flat list of values is not a loss, and what it wraps
    /// must match. Decisive: the length check, and the wrapping kept to
    /// lists of values.
    #[test]
    fn a_list_that_loses_an_element_is_refused() {
        let dropped = |request: serde_json::Value, sent: serde_json::Value| {
            dropped_key(&request, &sent, "template")
        };
        // An element (with its keys) lost.
        assert_eq!(
            dropped(
                json!({"buttons": [{"type": "url"}, {"type": "url", "app_deep_link": {}}]}),
                json!({"buttons": [{"type": "url"}]}),
            )
            .as_deref(),
            Some("template.buttons")
        );
        assert_eq!(
            dropped(json!({"v": ["a", "b"]}), json!({"v": ["a"]})).as_deref(),
            Some("template.v")
        );
        assert_eq!(
            dropped(json!(["a"]), json!(["a", "b"])).as_deref(),
            Some("template")
        );
        assert_eq!(
            dropped_key(&json!([1, 2]), &json!([1]), ""),
            Some("body".to_owned())
        );
        // A wrapped flat list must still hold every value.
        assert_eq!(
            dropped(json!({"v": ["a", "b", "c"]}), json!({"v": [["a", "b"]]})).as_deref(),
            Some("template.v")
        );
        assert_eq!(
            dropped(json!({"v": ["a", "b"]}), json!({"v": [["a", "b"]]})),
            None
        );
        // Objects are never taken for a list of values: one wrapped is
        // compared as it stands.
        assert_eq!(
            dropped(
                json!({"v": [{"x": 1}, {"y": 2}]}),
                json!({"v": [[{"x": 1}, {"y": 2}]]}),
            )
            .as_deref(),
            Some("template.v")
        );
    }

    /// A key holding nothing (`[]`, `null`, `{}`) that the library leaves
    /// out loses nothing: accepted. The same key holding something is
    /// refused. Decisive: the exemption of empty values.
    #[test]
    fn a_key_holding_nothing_may_be_left_out() {
        let send = json!({"name": "hello_world", "language": {"code": "en_US"}, "components": []});
        let template = meta_object::<TemplateMessage>("template", &send).unwrap();
        assert!(
            serde_json::to_value(&template)
                .unwrap()
                .get("components")
                .is_none(),
            "the library leaves an empty list out"
        );
        let mut definition = json!({"name": "order_update", "language": "en_US",
            "category": "UTILITY", "sub_category": null, "parameter_format": null,
            "components": [{"type": "body", "text": "Your order shipped."}]});
        assert_eq!(refused::<TemplateDefinition>("", &definition), None);
        definition["extra"] = json!({});
        assert_eq!(refused::<TemplateDefinition>("", &definition), None);
        definition["extra"] = json!({"a": 1});
        assert_eq!(
            refused::<TemplateDefinition>("", &definition).as_deref(),
            Some("extra")
        );
    }
}
