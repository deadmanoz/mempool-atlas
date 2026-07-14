use std::error::Error;
use std::fs;
use std::path::PathBuf;

use atlas_agent::peer_observer::wire::ebpf_extractor::Ebpf;
use atlas_agent::peer_observer::wire::ebpf_extractor::ebpf;
use atlas_agent::peer_observer::wire::ebpf_extractor::mempool::{
    Added, MempoolEvent, mempool_event,
};
use atlas_agent::peer_observer::wire::event::Event;
use atlas_agent::peer_observer::wire::event::event::PeerObserverEvent;
use prost::Message;

fn main() -> Result<(), Box<dyn Error>> {
    let event = Event {
        timestamp: 1_721_234_567_890,
        peer_observer_event: Some(PeerObserverEvent::EbpfExtractor(Ebpf {
            ebpf_event: Some(ebpf::EbpfEvent::Mempool(MempoolEvent {
                event: Some(mempool_event::Event::Added(Added {
                    txid: (0..32).collect(),
                    vsize: 453,
                    fee: 123,
                })),
            })),
        })),
    };

    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/peer-observer/mempool-added.pb");
    fs::create_dir_all(output.parent().expect("fixture parent"))?;
    fs::write(&output, event.encode_to_vec())?;
    println!("wrote {}", output.display());
    Ok(())
}
