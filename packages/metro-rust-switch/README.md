# metro-rust-switch

Chooses, per component, between Metro's JavaScript implementation and the Rust
one in `metro-rs/`. See `RULES.md` section 4 of the repository.

- `METRO_RUST=off|all|id1,id2` (environment) wins over
- `unstable_rust: {[componentId]: boolean}` (Metro config).
- Unset means JavaScript.

```js
const {getRustBinding} = require('metro-rust-switch');
const rust = getRustBinding('resolver', config.unstable_rust);
if (rust != null) { /* call into Rust */ } else { /* JavaScript */ }
```

The native module is loaded lazily and a load failure throws.
