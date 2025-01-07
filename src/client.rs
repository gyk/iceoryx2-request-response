use std::io::{self, stdin, BufRead};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{bounded, select, Receiver};
use iceoryx2::{
    port::{
        listener::Listener, notifier::Notifier, publisher::Publisher, subscriber::Subscriber,
        update_connections::UpdateConnections,
    },
    prelude::*,
    sample::Sample as IpcSample,
};

use crate::common::*;
use crate::events::IpcEvent;
use crate::messages::*;

const DEADLINE: Duration = Duration::from_secs(30);

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (ctrlc_tx, ctrlc_rx) = bounded(0);
    ctrlc::set_handler(move || {
        println!("Ctrl+C pressed!");
        ctrlc_tx
            .send(())
            .expect("Could not send signal on channel.")
    })
    .expect("Error setting Ctrl+C handler");

    let stdin_rx = spawn_stdin_chan();

    let node = NodeBuilder::new().create::<ipc::Service>()?;
    let ipc_client = IpcClient::new(&node, stdin_rx, ctrlc_rx)?;

    let waitset = WaitSetBuilder::new()
        .signal_handling_mode(SignalHandlingMode::Disabled)
        .create::<ipc::Service>()?;
    let client_guard = waitset.attach_notification(&ipc_client)?;

    let mut on_event = |attachment_id: WaitSetAttachmentId<ipc::Service>| {
        if attachment_id.has_event_from(&client_guard) {
            match ipc_client.handle_event() {
                Ok(ControlFlow::Break(_)) => return CallbackProgression::Stop,
                _ => (),
            }
        } else if attachment_id.has_missed_deadline(&client_guard) {
            println!(
                "⚠️ The server did not respond a message for {:?}.",
                DEADLINE
            );
            return CallbackProgression::Stop;
        }

        CallbackProgression::Continue
    };

    waitset.wait_and_process(&mut on_event)?;

    println!("exit");
    Ok(())
}

#[derive(Debug)]
struct IpcClient {
    // IPC
    publisher: Publisher<ipc::Service, [u8], ()>,
    subscriber: Subscriber<ipc::Service, [u8], ()>,
    listener: Listener<ipc::Service>,
    notifier: Notifier<ipc::Service>,

    // State
    is_server_running: AtomicBool,

    // User input
    stdin_rx: Receiver<io::Result<String>>,
    ctrlc_rx: Receiver<()>,
}

impl FileDescriptorBased for IpcClient {
    fn file_descriptor(&self) -> &FileDescriptor {
        self.listener.file_descriptor()
    }
}

impl SynchronousMultiplexing for IpcClient {}

impl IpcClient {
    fn new(
        node: &Node<ipc::Service>,
        stdin_rx: Receiver<io::Result<String>>,
        ctrlc_rx: Receiver<()>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let s2c_service_name: ServiceName = S2C_SERVICE_NAME.try_into()?;
        let s2c_service = node
            .service_builder(&s2c_service_name)
            .publish_subscribe::<[u8]>()
            .history_size(HISTORY_SIZE)
            .subscriber_max_buffer_size(HISTORY_SIZE)
            .open_or_create()
            .unwrap();
        let event_service_name = EVENT_SERVICE_NAME.try_into()?;
        let event_service = node
            .service_builder(&event_service_name)
            .event()
            .open_or_create()?;

        let listener = event_service.listener_builder().create()?;
        let notifier = event_service.notifier_builder().create()?;
        let subscriber = s2c_service.subscriber_builder().create()?;

        let c2s_service_name: ServiceName = C2S_SERVICE_NAME.try_into()?;
        let c2s_service = node
            .service_builder(&c2s_service_name)
            .publish_subscribe::<[u8]>()
            .history_size(HISTORY_SIZE)
            .subscriber_max_buffer_size(HISTORY_SIZE)
            .open_or_create()?;

        let publisher = c2s_service
            .publisher_builder()
            .initial_max_slice_len(SLICE_SIZE_HINT)
            .allocation_strategy(AllocationStrategy::PowerOfTwo)
            .create()?;

        notifier.notify_with_custom_event_id(IpcEvent::ClientConnected.into())?;

        Ok(Self {
            publisher,
            subscriber,
            listener,
            notifier,

            is_server_running: AtomicBool::new(false),

            stdin_rx,
            ctrlc_rx,
        })
    }

    fn handle_event(&self) -> Result<ControlFlow<()>, Box<dyn std::error::Error>> {
        println!(">>> Start handle_event");
        while let Some(event) = self.listener.try_wait_one()? {
            let event: IpcEvent = event.into();
            match event {
                IpcEvent::ServerConnected => {
                    println!("Server connected");
                    self.publisher.update_connections().unwrap();
                    self.is_server_running.store(true, Ordering::SeqCst);
                }
                IpcEvent::ServerDisconnected => {
                    println!("Server disconnected");
                    self.is_server_running.store(false, Ordering::SeqCst);
                }
                IpcEvent::RequestReceived => {
                    println!("Server has received request");
                }
                IpcEvent::ResponseSent => {
                    if let Ok(Some(sample)) = self.receive() {
                        println!("RESP received: len = {}", sample.payload().len());

                        let response =
                            bincode::deserialize::<ResponseRef>(sample.payload()).unwrap();

                        match response {
                            ResponseRef::FileSize(size) => {
                                println!("File size = {}", size);
                            }
                            ResponseRef::FileContent(data) => {
                                println!("File content: len = {}", data.len());
                                println!("Head: {:02X?}", &data[..data.len().min(8)]);
                            }
                        }
                    }
                }
                IpcEvent::ServerReady => {
                    self.is_server_running.store(true, Ordering::SeqCst);
                    println!("\nPlease input the path to file:");
                    if let Some(input) = read_line(&self.stdin_rx, &self.ctrlc_rx) {
                        let path = PathBuf::from(input);
                        let request = Request::GetFileContent { path };
                        let bytes = bincode::serialize(&request).unwrap();
                        let _ = self.send(&bytes);
                    } else {
                        return Ok(ControlFlow::Break(()));
                    }
                }
                _ => (),
            }
        }
        println!("<<< End handle_event");

        Ok(ControlFlow::Continue(()))
    }

    fn send(&self, data: &[u8]) -> Result<()> {
        println!("📤 Client send {}", data.len());
        let sample = self.publisher.loan_slice_uninit(data.len())?;
        let sample = sample.write_from_slice(data);
        sample.send()?;

        self.notifier
            .notify_with_custom_event_id(IpcEvent::RequestSent.into())?;
        Ok(())
    }

    fn receive(
        &self,
    ) -> Result<Option<IpcSample<ipc::Service, [u8], ()>>, Box<dyn std::error::Error>> {
        match self.subscriber.receive()? {
            Some(sample) => {
                self.notifier
                    .notify_with_custom_event_id(IpcEvent::ResponseReceived.into())?;
                Ok(Some(sample))
            }
            None => Ok(None),
        }
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        let _ = self
            .notifier
            .notify_with_custom_event_id(IpcEvent::ClientDisconnected.into());
    }
}

// ===== User input & Signal handling ===== //

fn spawn_stdin_chan() -> Receiver<io::Result<String>> {
    let (tx, rx) = bounded(0);
    thread::spawn(move || {
        let stdin = stdin();
        for line in stdin.lock().lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn read_line(stdin_rx: &Receiver<io::Result<String>>, ctrlc_rx: &Receiver<()>) -> Option<String> {
    select! {
        recv(ctrlc_rx) -> _signal => {
            eprintln!("Ctrl+C pressed");
            None
        },
        recv(stdin_rx) -> line => match line {
            Ok(line) => line.ok(),
            Err(_) => None,
        }
    }
}
