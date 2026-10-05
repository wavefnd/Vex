# A library and application

Install Vex and the verified Wave `0.2.1-pre-beta` compiler using the
[installation guide](../README.md#install). Start in an empty working directory:

```sh
mkdir greeting app
cd greeting
vex init --lib
cd ../app
vex init
```

Keep the generated `greeting/src/lib.wave`: its `pub fun greet()` exports the
function across the package boundary. Set the application's `vex.ws` to:

```wson
{
    format = 2,
    name = "app",
    version = 0.1.0,
    compiler = "0.2.1-pre-beta",
    dependencies = [{ name = "greeting", path = "../greeting" }]
}
```

Set `app/src/main.wave` to:

```wave
import("greeting")::{greet};
fun main() { greet(); }
```

From `app`, resolve and run:

```sh
vex fetch
vex run --locked --offline
```

The output includes `Hello from library`. Commit `vex.ws`, `vex.lock`, and your
source files. The lockfile records the dependency graph; `--locked` refuses an
outdated graph and `--offline` refuses network access. Path dependency source
contents remain your responsibility to version with the project. Private functions
cannot be imported. Neither a registry nor raw compiler flags are needed.

`tests/wave_compatibility.py` exercises this scaffold alongside a transitive local
Git dependency, re-exports, and private-symbol rejection using the official compiler.
