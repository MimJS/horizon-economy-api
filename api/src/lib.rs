//! The economy's public contract, in the style of Bukkit's Vault.
//!
//! A mod that needs money depends only on this crate (`economy_api = { version = "^1", optional =
//! true }` in its `horizon-mixin.toml`) and asks [`provider()`] for whatever economy the server
//! runs. An economy implements [`Economy`] and calls [`register`] once, from its `hooks()`; the
//! game calls `hooks()` in dependency order, so the provider is in place before any mod that
//! depends on this one runs a hook.
//!
//! Compatibility: 1.x only adds. New trait methods get a default body, the error and event types
//! are `#[non_exhaustive]`, so a mod built against 1.0 keeps building against any 1.x.
//!
//! Threading: hooks run on the server thread, but mods share one process, so everything here is
//! `Send + Sync`. Listeners are called after the provider has released its own locks, so a
//! listener may read balances (and even move money) without deadlocking.

use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use horizon_client::mod_api::Hooks;

/// A player's UUID, as Horizon's hooks give it (`NativeServerPlayer::uuid`).
pub type PlayerId = u128;

/// An amount of money, in whole units of the currency. Never a float.
pub type Amount = i64;

/// The version of this contract; bumped with each minor addition (1.0 → 1, 1.1 → 2...).
pub const API_VERSION: u32 = 1;

/// Why an operation on the economy did nothing. Nothing changes when one is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EcoError {
    /// No economy mod is installed (or none registered yet).
    NoProvider,
    /// The player has no account: never joined this server.
    UnknownPlayer,
    /// Zero, negative, or otherwise not accepted for this operation.
    InvalidAmount,
    /// A withdrawal or transfer larger than the balance.
    InsufficientFunds { balance: Amount, needed: Amount },
    /// The result would not fit in an [`Amount`] (or exceed the provider's cap).
    Overflow,
    /// A transfer from a player to themselves.
    SameAccount,
    /// The world is not open yet, so there is nowhere to keep the money.
    NotReady,
    /// The provider's own failure (it could not write its files...).
    Provider(String),
}

impl fmt::Display for EcoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProvider => f.write_str("no economy is installed"),
            Self::UnknownPlayer => f.write_str("the player has no account"),
            Self::InvalidAmount => f.write_str("the amount must be positive"),
            Self::InsufficientFunds { balance, needed } => {
                write!(f, "not enough money: {balance} of {needed}")
            }
            Self::Overflow => f.write_str("the balance would overflow"),
            Self::SameAccount => f.write_str("cannot transfer to the same account"),
            Self::NotReady => f.write_str("the economy is not ready (no world open)"),
            Self::Provider(e) => write!(f, "economy error: {e}"),
        }
    }
}

impl std::error::Error for EcoError {}

/// What moved a balance. Providers fill it; listeners read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeKind {
    Deposit,
    Withdraw,
    /// One side of a transfer; `counterparty` says who the other side is.
    Transfer,
    /// An administrator (or a mod) set the balance outright.
    Set,
    /// A new account opened with its starting balance.
    Opened,
}

/// One balance that changed, published to [`on_change`] listeners after it is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct BalanceChange {
    pub player: PlayerId,
    pub old: Amount,
    pub new: Amount,
    pub kind: ChangeKind,
    /// For a transfer: the other account.
    pub counterparty: Option<PlayerId>,
    /// Who asked and why, `mod_id:what` by convention (`shop:buy`, `economy:pay`).
    pub reason: String,
}

impl BalanceChange {
    pub fn new(
        player: PlayerId,
        old: Amount,
        new: Amount,
        kind: ChangeKind,
        counterparty: Option<PlayerId>,
        reason: &str,
    ) -> Self {
        Self {
            player,
            old,
            new,
            kind,
            counterparty,
            reason: reason.to_owned(),
        }
    }

    /// `new - old`.
    pub fn delta(&self) -> Amount {
        self.new.saturating_sub(self.old)
    }
}

/// An economy: accounts by player, and the operations every caller needs.
///
/// `reason` is free text for the provider's audit log, `mod_id:what` by convention. Every method
/// either changes everything it says or nothing (no half transfers).
pub trait Economy: Send + Sync + 'static {
    /// The implementation's name, for logs (`economy`).
    fn name(&self) -> &str;

    /// An amount as players read it, with the currency (`1 250 монет`).
    fn format(&self, amount: Amount) -> String {
        amount.to_string()
    }

    /// Whether the player has an account.
    fn has_account(&self, player: PlayerId) -> bool {
        self.balance(player).is_some()
    }

    /// The player's balance; `None` when they have no account.
    fn balance(&self, player: PlayerId) -> Option<Amount>;

    /// The player whose account has this name (the name last seen, any case), online or not.
    fn find_player(&self, name: &str) -> Option<PlayerId>;

    /// The name last seen for the account.
    fn player_name(&self, player: PlayerId) -> Option<String>;

    /// Adds `amount` (> 0); returns the new balance.
    fn deposit(&self, player: PlayerId, amount: Amount, reason: &str) -> Result<Amount, EcoError>;

    /// Takes `amount` (> 0) if the balance covers it; returns the new balance.
    fn withdraw(&self, player: PlayerId, amount: Amount, reason: &str) -> Result<Amount, EcoError>;

    /// Moves `amount` (> 0) from one account to another, both or neither.
    fn transfer(
        &self,
        from: PlayerId,
        to: PlayerId,
        amount: Amount,
        reason: &str,
    ) -> Result<(), EcoError>;

    /// Sets the balance outright (>= 0); returns the old one.
    fn set_balance(&self, player: PlayerId, amount: Amount, reason: &str)
    -> Result<Amount, EcoError>;

    /// The richest accounts, richest first, at most `limit`. Providers may leave it empty.
    fn top(&self, limit: usize) -> Vec<(PlayerId, Amount)> {
        let _ = limit;
        Vec::new()
    }
}

static PROVIDER: OnceLock<&'static dyn Economy> = OnceLock::new();

type Listener = Arc<dyn Fn(&BalanceChange) + Send + Sync>;
static LISTENERS: RwLock<Vec<Listener>> = RwLock::new(Vec::new());

/// Installs the server's economy. Only the first registration counts: a second economy is
/// refused with the name of the one in place, so two economies never split the money.
pub fn register(provider: Box<dyn Economy>) -> Result<(), String> {
    let provider: &'static dyn Economy = Box::leak(provider);
    PROVIDER.set(provider).map_err(|_| {
        format!(
            "an economy is already registered ({}); {} was refused",
            PROVIDER.get().map_or("?", |p| p.name()),
            provider.name()
        )
    })
}

/// The server's economy, if one is installed.
pub fn provider() -> Option<&'static dyn Economy> {
    PROVIDER.get().copied()
}

/// The server's economy, or [`EcoError::NoProvider`]: for `?` in a mod's code.
pub fn require() -> Result<&'static dyn Economy, EcoError> {
    provider().ok_or(EcoError::NoProvider)
}

/// Calls `listener` after every balance change, whichever mod made it. Register from `hooks()`.
pub fn on_change(listener: impl Fn(&BalanceChange) + Send + Sync + 'static) {
    LISTENERS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::new(listener));
}

/// For providers: tells every listener about changes already stored. Call it with no lock held.
pub fn publish(changes: &[BalanceChange]) {
    // A snapshot, so a listener may itself call `on_change` or move money.
    let listeners: Vec<Listener> = LISTENERS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    for change in changes {
        for listener in &listeners {
            listener(change);
        }
    }
}

/// Shorthand: the balance from the installed economy (`None`: no economy or no account).
pub fn balance(player: PlayerId) -> Option<Amount> {
    provider()?.balance(player)
}

/// Shorthand: an amount formatted by the installed economy, or the bare number without one.
pub fn format(amount: Amount) -> String {
    provider().map_or_else(|| amount.to_string(), |p| p.format(amount))
}

/// This crate is a mod of its own, with nothing to hook.
pub fn hooks() -> Box<dyn Hooks> {
    Box::new(Contract)
}

struct Contract;

impl Hooks for Contract {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_is_new_minus_old() {
        let change = BalanceChange::new(1, 100, 40, ChangeKind::Withdraw, None, "test:x");
        assert_eq!(change.delta(), -60);
    }

    #[test]
    fn listeners_see_published_changes_and_may_subscribe_inside() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        on_change(|c| {
            SEEN.fetch_add(c.new as usize, Ordering::SeqCst);
            // Subscribing from a listener must not deadlock.
            on_change(|_| {});
        });
        publish(&[BalanceChange::new(1, 0, 5, ChangeKind::Deposit, None, "test:x")]);
        assert_eq!(SEEN.load(Ordering::SeqCst), 5);
    }
}
