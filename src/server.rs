use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;

use iceoryx2::{
    port::{
        listener::Listener, notifier::Notifier, publisher::Publisher, subscriber::Subscriber,
        update_connections::UpdateConnections,
    },
    prelude::*,
    sample::Sample as IpcSample,
    service::builder::publish_subscribe::PublishSubscribeOpenOrCreateError,
};

const DEADLINE: Duration = Duration::from_secs(15);

use crate::common::*;
use crate::events::IpcEvent;
use crate::messages::*;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let ipc_server = match IpcServer::new(&node) {
        Ok(server) => server,
        Err(err) => {
            match err.downcast::<PublishSubscribeOpenOrCreateError>() {
                Ok(pub_sub_err) => {
                    let config = node.config();
                    // FIXME: `cleanup_dead_nodes` does not work on Windows.
                    let res = iceoryx2::node::Node::<ipc::Service>::cleanup_dead_nodes(config);
                    println!("cleanup_dead_nodes: {:?}", res);
                    return Err(pub_sub_err);
                }
                Err(err) => return Err(err),
            }
        }
    };

    let waitset = WaitSetBuilder::new().create::<ipc::Service>()?;

    let server_guard = waitset.attach_deadline(&ipc_server, DEADLINE)?;

    let on_event = |attachment_id: WaitSetAttachmentId<ipc::Service>| {
        if attachment_id.has_event_from(&server_guard) {
            ipc_server.handle_event().unwrap();
        } else if attachment_id.has_missed_deadline(&server_guard) {
            if !ipc_server.has_client.load(Ordering::SeqCst) {
                return CallbackProgression::Stop;
            }

            println!(
                "⚠️ The subscriber did not receive a message for {:?}.",
                DEADLINE
            );
        }

        CallbackProgression::Continue
    };

    waitset.wait_and_process(on_event)?;

    println!("exit");
    Ok(())
}

#[derive(Debug)]
struct IpcServer {
    has_client: AtomicBool,
    subscriber: Subscriber<ipc::Service, [u8], ()>,
    publisher: Publisher<ipc::Service, [u8], ()>,
    notifier: Notifier<ipc::Service>,
    listener: Listener<ipc::Service>,
}

impl FileDescriptorBased for IpcServer {
    fn file_descriptor(&self) -> &FileDescriptor {
        self.listener.file_descriptor()
    }
}

impl SynchronousMultiplexing for IpcServer {}

impl IpcServer {
    fn new(node: &Node<ipc::Service>) -> Result<Self, Box<dyn std::error::Error>> {
        let c2s_service_name: ServiceName = C2S_SERVICE_NAME.try_into()?;
        let c2s_service = node
            .service_builder(&c2s_service_name)
            .publish_subscribe::<[u8]>()
            .history_size(HISTORY_SIZE)
            .subscriber_max_buffer_size(HISTORY_SIZE)
            .open_or_create()?;
        let event_service_name = EVENT_SERVICE_NAME.try_into()?;
        let event_service = node
            .service_builder(&event_service_name)
            .event()
            .open_or_create()?;

        let listener = event_service.listener_builder().create()?;
        let notifier = event_service.notifier_builder().create()?;
        let subscriber = c2s_service.subscriber_builder().create()?;

        let s2c_service_name: ServiceName = S2C_SERVICE_NAME.try_into()?;
        let s2c_service = node
            .service_builder(&s2c_service_name)
            .publish_subscribe::<[u8]>()
            .history_size(HISTORY_SIZE)
            .subscriber_max_buffer_size(HISTORY_SIZE)
            .open_or_create()?;

        let publisher = s2c_service
            .publisher_builder()
            .initial_max_slice_len(SLICE_SIZE_HINT)
            .allocation_strategy(AllocationStrategy::PowerOfTwo)
            .create()?;

        notifier.notify_with_custom_event_id(IpcEvent::ServerConnected.into())?;
        notifier.notify_with_custom_event_id(IpcEvent::ServerReady.into())?;

        Ok(Self {
            has_client: AtomicBool::new(false),
            subscriber,
            publisher,
            listener,
            notifier,
        })
    }

    fn handle_event(&self) -> Result<(), Box<dyn std::error::Error>> {
        while let Some(event) = self.listener.try_wait_one()? {
            let event: IpcEvent = event.into();
            match event {
                IpcEvent::RequestSent => {
                    if let Ok(Some(sample)) = self.receive() {
                        println!("received: len = {}", sample.payload().len());

                        let request = bincode::deserialize::<Request>(sample.payload()).unwrap();

                        match request {
                            Request::GetFileSize { path } => {
                                let size = std::fs::metadata(&path)?.len();
                                let response = Response::FileSize(size);
                                let data = bincode::serialize(&response).unwrap();
                                self.send(&data)?;
                            }
                            Request::GetFileContent { path } => {
                                let content = std::fs::read(&path)?;
                                let response = Response::FileContent(content);
                                let data = bincode::serialize(&response).unwrap();
                                self.send(&data)?;
                            }
                        }
                    }
                }
                IpcEvent::ClientConnected => {
                    println!("new client connected");
                    self.has_client.store(true, Ordering::SeqCst);
                    self.publisher.update_connections().unwrap();
                    self.notifier
                        .notify_with_custom_event_id(IpcEvent::ServerReady.into())?;
                }
                IpcEvent::ClientDisconnected => {
                    println!("client disconnected");
                    self.has_client.store(false, Ordering::SeqCst);
                }
                IpcEvent::ResponseReceived => {
                    self.notifier
                        .notify_with_custom_event_id(IpcEvent::ServerReady.into())?;
                }
                _ => (),
            }
        }

        Ok(())
    }

    fn receive(
        &self,
    ) -> Result<Option<IpcSample<ipc::Service, [u8], ()>>, Box<dyn std::error::Error>> {
        match self.subscriber.receive()? {
            Some(sample) => {
                self.notifier
                    .notify_with_custom_event_id(IpcEvent::RequestReceived.into())?;
                Ok(Some(sample))
            }
            None => Ok(None),
        }
    }

    fn send(&self, data: &[u8]) -> Result<()> {
        let sample = self.publisher.loan_slice_uninit(data.len())?;
        let sample = sample.write_from_slice(data);
        sample.send()?;

        self.notifier
            .notify_with_custom_event_id(IpcEvent::ResponseSent.into())?;
        Ok(())
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.notifier
            .notify_with_custom_event_id(IpcEvent::ServerDisconnected.into())
            .unwrap();
    }
}
