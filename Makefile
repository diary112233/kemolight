CARGO ?= cargo
BIN ?= kemolight
TARGET_DIR ?= target
RELEASE_BIN := $(TARGET_DIR)/release/$(BIN)
UPX ?= upx

FEATURE_FLAGS :=
ifeq ($(DEFAULT_FEATURES),0)
FEATURE_FLAGS += --no-default-features
endif
ifneq ($(strip $(FEATURES)),)
FEATURE_FLAGS += --features $(FEATURES)
endif

.PHONY: all fmt check test build run release run-release size compress clean

all: fmt check test release

fmt:
	$(CARGO) fmt -- --check

check:
	$(CARGO) check $(FEATURE_FLAGS)

test:
	$(CARGO) test $(FEATURE_FLAGS)

build:
	$(CARGO) build $(FEATURE_FLAGS)

run:
	$(CARGO) run $(FEATURE_FLAGS)

release:
	$(CARGO) build --release $(FEATURE_FLAGS)

run-release:
	$(CARGO) run --release $(FEATURE_FLAGS)

size: release
	@ls -lh $(RELEASE_BIN)
	@du -h $(RELEASE_BIN)
	@file $(RELEASE_BIN)

compress: release
	@if command -v $(UPX) >/dev/null 2>&1; then \
		cp $(RELEASE_BIN) $(RELEASE_BIN).upx; \
		$(UPX) --best --lzma $(RELEASE_BIN).upx; \
		ls -lh $(RELEASE_BIN) $(RELEASE_BIN).upx; \
	else \
		echo "UPX is not installed. Install it or use 'make size' for the stripped release binary."; \
		exit 1; \
	fi

clean:
	$(CARGO) clean
