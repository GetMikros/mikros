//! The `Session` channel of a user session's compositor: the session
//! manager asks the questions of the session's processes there. Each
//! request goes to the `prompts` component, which no system app provides
//! yet, so the compositor cancels every request as it comes
//! (MIKSPEC-0075.R24).

use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::session::{Session, SessionAsyncClient, SessionEvent};

/// Reads the requests on the `Session` channel `session` until it closes,
/// and cancels each.
pub async fn serve(reactor: Reactor, session: OwnedHandle) {
    let session: ClientEnd<Session> = ClientEnd::new(session);
    let session = SessionAsyncClient::new(session, reactor);
    while let Ok(event) = session.next_event().await {
        let request = match event {
            SessionEvent::OnAuthorize(e) => e.request,
            SessionEvent::OnAsk(e) => e.request,
            SessionEvent::OnAuthorizeEnded(_) => continue,
        };
        match session.cancel(request).await {
            Ok(Ok(_)) => mikros_rt::log!(
                Info,
                "compositor: cancelled the question {request}, as no prompts component runs"
            ),
            other => mikros_rt::log!(
                Error,
                "compositor: cancelling the question {request}: {other:?}"
            ),
        }
    }
}
