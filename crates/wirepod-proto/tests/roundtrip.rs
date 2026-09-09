//! Behavior tests: encoded messages survive a prost encode/decode roundtrip
//! with the field numbers the robot firmware expects.

use prost::Message;
use wirepod_proto::chippergrpc2::{
    ConnectionCheckResponse, IntentResult, StreamingConnectionCheckRequest,
};
use wirepod_proto::jdocspb::Jdoc;

#[test]
fn connection_check_response_roundtrip() {
    let req = StreamingConnectionCheckRequest {
        session: "sess".into(),
        device_id: "00303f28".into(),
        input_audio: vec![0u8; 3200],
        firmware_version: "1.6".into(),
        ..Default::default()
    };
    let req_back = StreamingConnectionCheckRequest::decode(req.encode_to_vec().as_slice()).unwrap();
    assert_eq!(req_back.device_id, "00303f28");
    let msg = ConnectionCheckResponse {
        frames_received: 42,
        status: "Success".into(),
    };
    let bytes = msg.encode_to_vec();
    let back = ConnectionCheckResponse::decode(bytes.as_slice()).unwrap();
    assert_eq!(back.status, "Success");
    assert_eq!(back.frames_received, 42);
}

#[test]
fn intent_result_parameters_map_roundtrip() {
    let mut params = std::collections::HashMap::new();
    params.insert("timer_duration".to_string(), "600".to_string());
    let msg = IntentResult {
        query_text: "set a timer for ten minutes".into(),
        action: "intent_clock_settimer_extend".into(),
        parameters: params,
        ..Default::default()
    };
    let back = IntentResult::decode(msg.encode_to_vec().as_slice()).unwrap();
    assert_eq!(back.parameters["timer_duration"], "600");
    assert_eq!(back.action, "intent_clock_settimer_extend");
}

#[test]
fn jdoc_roundtrip() {
    let doc = Jdoc {
        doc_version: 313,
        fmt_version: 1,
        client_metadata: "placeholder".into(),
        json_doc: r#"{"BStat.NumWakeups":1}"#.into(),
    };
    let back = Jdoc::decode(doc.encode_to_vec().as_slice()).unwrap();
    assert_eq!(back.doc_version, 313);
    assert_eq!(back.json_doc, doc.json_doc);
}
