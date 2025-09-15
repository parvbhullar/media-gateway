use super::registration::RegistrationHandle;
use crate::call::sip::Invitation;
use crate::config::UseragentConfig;
use crate::useragent::invitation::{
    InvitationHandler, PendingDialog, UnavailableInvitationHandler,
};
use anyhow::{Result, anyhow};
use humantime::parse_duration;
use rsip::prelude::HeadersExt;
use rsipstack::EndpointBuilder;
use rsipstack::dialog::dialog_layer::DialogLayer;
use rsipstack::transaction::endpoint::EndpointOption;
use rsipstack::transaction::{Endpoint, TransactionReceiver};
use rsipstack::transport::{TransportLayer, udp::UdpConnection};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub struct UserAgentBuilder {
    pub config: Option<UseragentConfig>,
    pub cancel_token: Option<CancellationToken>,
    pub invitation_handler: Option<Box<dyn InvitationHandler>>,
}

pub struct UserAgent {
    pub config: UseragentConfig,
    pub token: CancellationToken,
    pub endpoint: Endpoint,
    pub registration_handles: Mutex<HashMap<String, RegistrationHandle>>,
    pub alive_users: Arc<RwLock<HashSet<String>>>,
    pub dialog_layer: Arc<DialogLayer>,
    pub invitation_handler: Box<dyn InvitationHandler>,
    pub invitation: Invitation,
}

impl UserAgentBuilder {
    pub fn new() -> Self {
        Self {
            config: None,
            cancel_token: None,
            invitation_handler: None,
        }
    }
    pub fn with_config(mut self, config: Option<UseragentConfig>) -> Self {
        self.config = config;
        self
    }

    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }
    pub fn with_invitation_handler(mut self, handler: Option<Box<dyn InvitationHandler>>) -> Self {
        self.invitation_handler = handler;
        self
    }

    pub async fn build(mut self) -> Result<UserAgent> {
        let cancel_token = self
            .cancel_token
            .take()
            .unwrap_or_else(|| CancellationToken::new());

        let config = self.config.to_owned().unwrap_or_default();
        let local_ip = if !config.addr.is_empty() {
            IpAddr::from_str(config.addr.as_str())?
        } else {
            crate::net_tool::get_first_non_loopback_interface()?
        };
        let transport_layer = TransportLayer::new(cancel_token.clone());
        let local_addr: SocketAddr = format!("{}:{}", local_ip, config.udp_port).parse()?;

        let udp_conn =
            UdpConnection::create_connection(local_addr, None, Some(cancel_token.child_token()))
                .await
                .map_err(|e| anyhow!("Create useragent UDP connection: {} {}", local_addr, e))?;

        transport_layer.add_transport(udp_conn.into());
        info!("start useragent, addr: {}", local_addr);

        let endpoint_option = EndpointOption {
            callid_suffix: config.callid_suffix.clone(),
            ..Default::default()
        };

        let endpoint = EndpointBuilder::new()
            .with_cancel_token(cancel_token.child_token())
            .with_transport_layer(transport_layer)
            .with_option(endpoint_option)
            .build();
        let dialog_layer = Arc::new(DialogLayer::new(endpoint.inner.clone()));

        Ok(UserAgent {
            token: cancel_token,
            config,
            endpoint,
            registration_handles: Mutex::new(HashMap::new()),
            alive_users: Arc::new(RwLock::new(HashSet::new())),
            dialog_layer: dialog_layer.clone(),
            invitation_handler: self
                .invitation_handler
                .take()
                .unwrap_or_else(|| Box::new(UnavailableInvitationHandler)),
            invitation: Invitation::new(dialog_layer),
        })
    }
}

impl UserAgent {
    async fn process_incoming_request(
        &self,
        dialog_layer: Arc<DialogLayer>,
        mut incoming: TransactionReceiver,
    ) -> Result<()> {
        while let Some(mut tx) = incoming.recv().await {
            let key: &rsipstack::transaction::key::TransactionKey = &tx.key;
            info!(?key, "received transaction");
            match tx.original.to_header()?.tag()?.as_ref() {
                Some(_) => match dialog_layer.match_dialog(&tx.original) {
                    Some(mut d) => {
                        tokio::spawn(async move {
                            match d.handle(&mut tx).await {
                                Ok(_) => (),
                                Err(e) => {
                                    info!("error handling transaction: {:?}", e);
                                }
                            }
                        });
                        continue;
                    }
                    None => {
                        info!("dialog not found: {}", tx.original);
                        match tx
                            .reply(rsip::StatusCode::CallTransactionDoesNotExist)
                            .await
                        {
                            Ok(_) => (),
                            Err(e) => {
                                info!("error replying to request: {:?}", e);
                            }
                        }
                        continue;
                    }
                },
                None => {}
            }
            // out dialog, new server dialog
            let (state_sender, state_receiver) = unbounded_channel();
            match tx.original.method {
                rsip::Method::Invite | rsip::Method::Ack => {
                    let contact = match dialog_layer.endpoint.get_addrs().first() {
                        Some(addr) => Some(rsip::Uri {
                            scheme: Some(rsip::Scheme::Sip),
                            auth: None,
                            host_with_port: addr.addr.clone(),
                            params: vec![],
                            headers: vec![],
                        }),
                        None => None,
                    };
                    let dialog = match dialog_layer.get_or_create_server_invite(
                        &tx,
                        state_sender,
                        None,
                        contact,
                    ) {
                        Ok(d) => d,
                        Err(e) => {
                            // 481 Dialog/Transaction Does Not Exist
                            info!("failed to obtain dialog: {:?}", e);
                            match tx
                                .reply(rsip::StatusCode::CallTransactionDoesNotExist)
                                .await
                            {
                                Ok(_) => (),
                                Err(e) => {
                                    info!("error replying to request: {:?}", e);
                                }
                            }
                            continue;
                        }
                    };
                    info!(id=?dialog.id(), "create server dialog");

                    let token = self.token.child_token();
                    let pending_dialog = PendingDialog {
                        token: token.clone(),
                        dialog: dialog.clone(),
                        state_receiver,
                    };
                    let dialog_id_str = dialog.id().to_string();
                    self.invitation
                        .pending_dialogs
                        .lock()
                        .await
                        .insert(dialog_id_str.clone(), pending_dialog);

                    let accept_timeout = self
                        .config
                        .accept_timeout
                        .as_ref()
                        .map(|t| parse_duration(t).ok())
                        .flatten()
                        .unwrap_or_else(|| Duration::from_secs(60));
                    let pending_dialogs = self.invitation.pending_dialogs.clone();
                    let dialog_id = dialog.id();
                    let token_ref = token.clone();

                    tokio::spawn(async move {
                        select! {
                            _ = token_ref.cancelled() => {}
                            _ = tokio::time::sleep(accept_timeout) => {}
                        }
                        if let Some(call) = pending_dialogs.lock().await.remove(&dialog_id_str) {
                            info!(?dialog_id, timeout = ?accept_timeout, "accept timeout, rejecting dialog");
                            // call.dialog.reject().ok();
                            call.dialog.reject(
                                    Some(rsip::StatusCode::BusyHere), 
                                    Some("Call Rejected".to_string())
                                ).ok();
                            
                            token_ref.cancel();
                        }
                    });
                    let mut dialog_ref = dialog.clone();
                    let token_ref = token.clone();

                    tokio::spawn(async move {
                        select! {
                            _ = token_ref.cancelled() => {}
                            _ = dialog_ref.handle(&mut tx) => {
                            }
                        }
                    });

                    match self
                        .invitation_handler
                        .on_invite(token, dialog.clone())
                        .await
                    {
                        Ok(_) => (),
                        Err(e) => {
                            info!(
                                id = ?dialog.id(),
                                "error handling invite: {:?}", e);
                            // dialog.reject().ok();
                            dialog.reject(
                                Some(rsip::StatusCode::BusyHere), 
                                Some("Call Rejected".to_string())
                            ).ok();
                        }
                    }
                }
                rsip::Method::Options => {
                    if tx.endpoint_inner.option.ignore_out_of_dialog_option {
                        let to_tag = tx
                            .original
                            .to_header()
                            .and_then(|to| to.tag())
                            .ok()
                            .flatten();
                        if to_tag.is_none() {
                            info!(?key, "ignoring out-of-dialog OPTIONS request");
                            continue;
                        }
                    }
                }
                _ => {
                    info!(?key, "received request: {:?}", tx.original.method);
                    match tx.reply(rsip::StatusCode::OK).await {
                        Ok(_) => (),
                        Err(e) => {
                            info!("error replying to request: {:?}", e);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn serve(&self) -> Result<()> {
        let incoming_txs = self.endpoint.incoming_transactions()?;
        let token = self.token.child_token();
        let endpoint_inner = self.endpoint.inner.clone();
        let dialog_layer = self.dialog_layer.clone();

        match self.start_registration().await {
            Ok(count) => {
                info!("registration started, count: {}", count);
            }
            Err(e) => {
                warn!("failed to start registration: {:?}", e);
            }
        }

        tokio::select! {
            _ = token.cancelled() => {
                info!("cancelled");
            }
            result = endpoint_inner.serve() => {
                if let Err(e) = result {
                    info!("endpoint serve error: {:?}", e);
                }
            }
            result = self.process_incoming_request(dialog_layer.clone(), incoming_txs) => {
                if let Err(e) = result {
                    info!("process incoming request error: {:?}", e);
                }
            },
        }

        // Wait for registration to stop, if not stopped within 50 seconds,
        // force stop it.
        let timeout = self
            .config
            .graceful_shutdown
            .map(|_| Duration::from_secs(50));

        match self.stop_registration(timeout).await {
            Ok(_) => {
                info!("registration stopped, waiting for clear");
            }
            Err(e) => {
                warn!("failed to stop registration: {:?}", e);
            }
        }
        info!("stopping");
        Ok(())
    }

    pub fn stop(&self) {
        info!("stopping");
        self.token.cancel();
    }
}
