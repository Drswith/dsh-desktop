# DSH Launcher — native macOS launcher for dsh that lives in the menu bar.
# Settings live in scripts/config.sh; override any of them per invocation,
# e.g. `make app ARCH=x86_64`; `make lock DSH_VERSION=…` pins another dsh.
APP_NAME ?= DSH Launcher
export

.PHONY: all lock payload app run dmg test clean distclean

all: app

# Pin the runtime to the submodule's dsh version (or DSH_VERSION=…) and refresh runtime/pnpm-lock.yaml.
lock:
	@scripts/update-lock.sh

payload:
	@scripts/prepare-payload.sh

app: payload
	@scripts/build-app.sh

run: app
	@open "build/$(APP_NAME).app"

dmg: app
	@scripts/make-dmg.sh

test:
	@swift test

clean:
	rm -rf build .build

distclean: clean
	rm -rf .cache
