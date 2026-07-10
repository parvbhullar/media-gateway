use crate::sip::prelude::{HeadersExt, ToTypedHeader};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;

use crate::{
    dialog::{
        dialog::DialogInner,
        server_dialog::ServerInviteDialog,
        tests::test_dialog_states::{create_invite_request, create_test_endpoint},
        DialogId,
    },
    transaction::{key::TransactionRole, transaction::TransactionEvent},
    transport::SipAddr,
};

#[tokio::test]
async fn test_dialog_make_request() -> crate::Result<()> {
    // Create dialog ID
    let dialog_id = DialogId {
        call_id: "test-call-id-123".to_string(),
        local_tag: "alice-tag-456".to_string(),
        remote_tag: "bob-tag-789".to_string(),
    };

    let endpoint = create_test_endpoint().await?;
    let (tu_sender, _tu_receiver) = unbounded_channel();
    let (state_sender, _state_receiver) = unbounded_channel();
    // Create INVITE request
    let invite_req = create_invite_request("alice-tag-456", "", "test-call-id-123");
    // Create dialog inner
    let dialog_inner = DialogInner::new(
        TransactionRole::Client,
        dialog_id.clone(),
        invite_req.clone(),
        endpoint.inner.clone(),
        state_sender,
        None,
        Some(crate::sip::Uri::try_from(
            "sip:alice@alice.example.com:5060",
        )?),
        tu_sender,
    )
    .expect("Failed to create dialog inner");

    let bye = dialog_inner
        .make_request(crate::sip::Method::Bye, None, None, None, None, None)
        .expect("Failed to make request");
    assert_eq!(bye.method, crate::sip::Method::Bye);

    assert!(bye
        .via_header()
        .expect("not via header")
        .typed()?
        .received()
        .is_none());
    assert!(
        bye.via_header().expect("not via header").typed()?.branch()
            != invite_req
                .via_header()
                .expect("not via header")
                .typed()?
                .branch()
    );
    Ok(())
}

#[tokio::test]
async fn test_accept_with_public_contact_preserves_contact_header() -> crate::Result<()> {
    // Create dialog ID
    let dialog_id = DialogId {
        call_id: "test-call-id-contact".to_string(),
        local_tag: "bob-tag-789".to_string(),
        remote_tag: "alice-tag-456".to_string(),
    };

    let endpoint = create_test_endpoint().await?;
    let (tu_sender, mut tu_receiver) = unbounded_channel();
    let (state_sender, _state_receiver) = unbounded_channel();

    // Create INVITE request
    let invite_req = create_invite_request(&dialog_id.remote_tag, "", "test-call-id-contact");

    // Create server dialog inner
    let dialog_inner = DialogInner::new(
        TransactionRole::Server,
        dialog_id.clone(),
        invite_req,
        endpoint.inner.clone(),
        state_sender,
        None,
        None,
        tu_sender,
    )
    .expect("Failed to create dialog inner");

    let server_dialog = ServerInviteDialog {
        inner: Arc::new(dialog_inner),
    };

    // Define the public address we want to use
    let public_address = Some(crate::sip::HostWithPort {
        host: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)).into(),
        port: Some(5060.into()),
    });

    // Define local address as fallback
    let local_address: SipAddr = crate::sip::HostWithPort::try_from("127.0.0.1:5060")?.into();

    // Accept with public contact
    server_dialog.accept_with_public_contact(
        "bob",
        public_address.clone(),
        &local_address,
        None,
        None,
    )?;

    // Receive the response from the transaction event channel
    let event = tu_receiver
        .recv()
        .await
        .expect("Should receive transaction event");

    match event {
        TransactionEvent::Respond(response) => {
            // Verify status code is 200 OK
            assert_eq!(response.status_code, crate::sip::StatusCode::OK);

            // Extract and verify Contact header
            let contact_header = response
                .contact_header()
                .expect("Response should have Contact header")
                .typed()
                .expect("Contact header should be parseable");

            // Verify the Contact URI matches the public address we provided
            assert_eq!(
                contact_header.uri.host_with_port.host,
                public_address.as_ref().unwrap().host
            );
            assert_eq!(
                contact_header.uri.host_with_port.port,
                public_address.as_ref().unwrap().port
            );

            // Verify the username in the Contact URI
            assert_eq!(contact_header.uri.auth.as_ref().unwrap().user, "bob");
        }
        _other => panic!("Expected TransactionEvent::Respond, got different event type"),
    }

    Ok(())
}

#[tokio::test]
async fn test_server_dialog_bye_via() -> crate::Result<()> {
    let dialog_id = DialogId {
        call_id: "server-bye-test".to_string(),
        local_tag: "bob-tag".to_string(),
        remote_tag: "alice-tag".to_string(),
    };

    let endpoint = create_test_endpoint().await?;
    let (tu_sender, _tu_receiver) = unbounded_channel();
    let (state_sender, _state_receiver) = unbounded_channel();

    // Create an INVITE request FROM alice (remote)
    // This invite has Via: SIP/2.0/UDP alice.example.com:5060;branch=...;received=172.0.0.1
    let invite_req = create_invite_request(&dialog_id.remote_tag, "", &dialog_id.call_id);

    let dialog_inner = DialogInner::new(
        TransactionRole::Server,
        dialog_id.clone(),
        invite_req.clone(),
        endpoint.inner.clone(),
        state_sender,
        None,
        None,
        tu_sender,
    )
    .expect("Failed to create dialog inner");

    let server_dialog = ServerInviteDialog {
        inner: Arc::new(dialog_inner),
    };

    // Generate BYE request
    let bye_req =
        server_dialog
            .inner
            .make_request(crate::sip::Method::Bye, None, None, None, None, None)?;

    assert_eq!(bye_req.method, crate::sip::Method::Bye);
    let via = bye_req.via_header().expect("no via").typed()?;

    // The Via in BYE should NOT have received=172.0.0.1 (which was in the incoming INVITE)
    assert!(via.received().is_none());

    // The host should be our local address (127.0.0.1) and NOT alice.example.com
    assert_eq!(via.uri.host_with_port.host.to_string(), "127.0.0.1");

    Ok(())
}

/// Regression: the caller-facing (UAS) BYE must be sent BEFORE the dialog is
/// marked Terminated.
///
/// The reverse ordering (transition-then-send) made the caller leg self-defeating:
/// the dialog flipped to Terminated before the BYE reached the wire, and — because
/// the dialog was already Terminated — every retry silently no-oped. This asserts
/// the invariant directly: after a BYE whose send does not complete successfully,
/// the dialog must NOT be left Terminated (it must stay Confirmed/retryable).
///
/// Here the send fails deterministically (the remote target has no transport type),
/// so `bye()` returns Err. Post-fix the dialog stays Confirmed (`terminated == false`);
/// pre-fix the transition-before-send marks it Terminated (`terminated == true`) — so
/// this fails on the buggy ordering and passes on the fix, verified both ways.
#[tokio::test]
async fn test_server_bye_does_not_terminate_before_send() -> crate::Result<()> {
    use crate::dialog::dialog::DialogState;

    let dialog_id = DialogId {
        call_id: "server-bye-order".to_string(),
        local_tag: "bob-tag".to_string(),
        remote_tag: "alice-tag".to_string(),
    };

    let endpoint = create_test_endpoint().await?;
    let (tu_sender, _tu_receiver) = unbounded_channel();
    let (state_sender, _state_receiver) = unbounded_channel();

    let invite_req = create_invite_request(&dialog_id.remote_tag, "", &dialog_id.call_id);
    let dialog_inner = DialogInner::new(
        TransactionRole::Server,
        dialog_id.clone(),
        invite_req,
        endpoint.inner.clone(),
        state_sender,
        None,
        None,
        tu_sender,
    )
    .expect("Failed to create dialog inner");

    // Point the remote target at an IP literal so the BYE resolves synchronously
    // (no DNS) and then blocks awaiting a response that never comes.
    *dialog_inner.remote_uri.lock() = crate::sip::Uri::try_from("sip:alice@127.0.0.1:65001")?;

    // Drive the dialog to Confirmed so bye() passes its is_confirmed() guard.
    dialog_inner.transition(DialogState::Confirmed(
        dialog_id.clone(),
        crate::sip::Response::default(),
    ))?;

    let server_dialog = ServerInviteDialog {
        inner: Arc::new(dialog_inner),
    };

    // Fire the BYE. The send does not complete (nothing is listening on
    // 127.0.0.1:65001), bounded so the test cannot hang.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        server_dialog.bye(),
    )
    .await;

    let terminated = server_dialog.state().is_terminated();

    // Invariant: unless the BYE actually completed successfully, the dialog must
    // NOT be marked Terminated — a failed/incomplete send must leave it Confirmed
    // and retryable. The buggy transition-before-send ordering marks it Terminated
    // regardless of whether the BYE ever reached the wire.
    let bye_succeeded = matches!(result, Ok(Ok(())));
    if !bye_succeeded {
        assert!(
            !terminated,
            "dialog was marked Terminated even though the BYE did not complete \
             (transition ran before the BYE was sent)"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_server_dialog_reinvite_via() -> crate::Result<()> {
    let dialog_id = DialogId {
        call_id: "server-reinvite-test".to_string(),
        local_tag: "bob-tag".to_string(),
        remote_tag: "alice-tag".to_string(),
    };

    let endpoint = create_test_endpoint().await?;
    let (tu_sender, _tu_receiver) = unbounded_channel();
    let (state_sender, _state_receiver) = unbounded_channel();

    let invite_req = create_invite_request(&dialog_id.remote_tag, "", &dialog_id.call_id);

    let dialog_inner = DialogInner::new(
        TransactionRole::Server,
        dialog_id.clone(),
        invite_req.clone(),
        endpoint.inner.clone(),
        state_sender,
        None,
        None,
        tu_sender,
    )
    .expect("Failed to create dialog inner");

    let server_dialog = ServerInviteDialog {
        inner: Arc::new(dialog_inner),
    };

    // Generate re-INVITE request
    let reinvite_req = server_dialog.inner.make_request(
        crate::sip::Method::Invite,
        None,
        None,
        None,
        None,
        None,
    )?;

    assert_eq!(reinvite_req.method, crate::sip::Method::Invite);
    let via = reinvite_req.via_header().expect("no via").typed()?;

    // Should not have mirrored the "received" param
    assert!(via.received().is_none());
    assert_eq!(via.uri.host_with_port.host.to_string(), "127.0.0.1");

    Ok(())
}
