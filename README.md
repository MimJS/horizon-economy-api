# economy_api

A shared economy contract for Horizon server mods, in the spirit of Bukkit's Vault.

It holds no money and adds no commands. It is the one place where an economy mod plugs in and where
every other mod (shops, jobs, auctions, `/profile` pages...) reads balances and moves money, without
knowing which economy the server runs.

## For mods that use money

`horizon-mixin.toml`:

```toml
[dependencies]
economy_api = { version = "^1", optional = true }   # or "^1" if your mod needs an economy
```

Don't add it to `Cargo.toml`: the launcher adds the path dependency, and, for an optional
dependency, a Cargo feature named `economy_api` that is on only when the mod is installed.

```rust
#[cfg(feature = "economy_api")]
fn buy(player: u128, price: i64) -> Result<(), economy_api::EcoError> {
    economy_api::require()?.withdraw(player, price, "shop:buy").map(|_| ())
}

// From your hooks(): hear every balance change, whichever mod made it.
economy_api::on_change(|change| { /* change.player, change.old, change.new, change.kind */ });
```

`economy_api::balance(uuid)` and `economy_api::format(amount)` are shorthands that work (returning
`None` or the bare number) when no economy is installed.

## For economy implementations

Implement `economy_api::Economy` and call `economy_api::register(Box::new(MyEconomy))` once, from
your `hooks()`. Only the first economy registered counts; a second one is refused with the name of
the first, so two economies never split the money. After storing changes, call
`economy_api::publish(&changes)` with no lock held.

## The contract

- Amounts are `i64` whole units, never floats. Players are their UUID (`u128`).
- Every operation changes everything it says or nothing (no half transfers), and returns an
  `EcoError` saying why when it does nothing.
- `reason` is free text for the provider's audit log, `mod_id:what` by convention.
- 1.x only adds: new trait methods get default bodies, and the error and event types are
  `#[non_exhaustive]`, so a mod built against 1.0 keeps building against any 1.x.
- Everything is `Send + Sync`. Listeners run after the provider releases its locks, so a listener
  may read balances or move money without deadlocking.

Built for Horizon Nightly 2026-11-08 (`fdc87ae`), Minecraft 26.3.

## License

MIT, see [LICENSE](LICENSE).
