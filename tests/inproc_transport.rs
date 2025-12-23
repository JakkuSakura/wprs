use wprs::protocols::wprs::serializer::RecvType;
use wprs::protocols::wprs::serializer::SendType;
use wprs::protocols::wprs::serializer::new_inproc_serializer_pair;
use wprs::protocols::wprs::types::Capabilities;
use wprs::protocols::wprs::types::Event;
use wprs::protocols::wprs::types::Request;

#[test]
fn inproc_forwards_object_messages() {
    let (server, mut client) = new_inproc_serializer_pair::<Request, Event>().unwrap();
    let caps = Capabilities { xwayland: false };

    server.writer().send(SendType::Object(Request::Capabilities(caps.clone())));

    let reader = client.reader().unwrap();
    let msg = reader.recv().unwrap();
    match msg {
        RecvType::Object(Request::Capabilities(received)) => {
            assert_eq!(received, caps);
        }
        other => panic!("unexpected recv type: {other:?}"),
    }
}
