//! Pending-start ownership for app-server requests that can start TUI work.

use crate::chatwidget::ChatWidget;
use std::future::Future;

/// Keep the pending-start UI active while one app-server request can start work.
///
/// A rejected request rolls back only the reservation acquired by that request, preserving an
/// older accepted request that is still awaiting its start notification.
pub(super) async fn dispatch_starting_request<T, E>(
    chat_widget: &mut ChatWidget,
    request: impl Future<Output = std::result::Result<T, E>>,
) -> std::result::Result<T, E> {
    let reservation = chat_widget.reserve_user_turn_pending_start();
    let result = request.await;
    if result.is_err() {
        chat_widget.rollback_user_turn_pending_start(reservation);
    }
    result
}
