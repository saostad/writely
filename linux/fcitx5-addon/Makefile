# Writely bridge add-on for fcitx5.
#
# Builds a fcitx5 module add-on that talks to the Writely process over the
# session D-Bus (protocol: docs/linux-ime-bridge.md in the Writely repo).

CXX ?= g++
PKG_CONFIG ?= pkg-config
PREFIX ?= /usr
DESTDIR ?=
LIBDIR ?= $(shell $(PKG_CONFIG) --variable=libdir Fcitx5Core)

CXXFLAGS += -std=c++20 -Wall -Wextra -Werror -fPIC \
	$(shell $(PKG_CONFIG) --cflags Fcitx5Core Fcitx5Utils Fcitx5Config Fcitx5Module)
LDLIBS := $(shell $(PKG_CONFIG) --libs Fcitx5Core Fcitx5Utils Fcitx5Config Fcitx5Module)

all: libwritely.so

libwritely.so: writely.o
	$(CXX) -shared -o $@ $^ $(LDLIBS)

writely.o: writely.cpp writely.h
	$(CXX) $(CXXFLAGS) -c -o $@ $<

install: libwritely.so
	install -d $(DESTDIR)$(LIBDIR)/fcitx5 $(DESTDIR)$(PREFIX)/share/fcitx5/addon
	install -m 644 libwritely.so $(DESTDIR)$(LIBDIR)/fcitx5/
	install -m 644 writely.conf $(DESTDIR)$(PREFIX)/share/fcitx5/addon/

install-user: libwritely.so
	install -d $(HOME)/.local/lib/fcitx5 $(HOME)/.local/share/fcitx5/addon
	install -m 644 libwritely.so $(HOME)/.local/lib/fcitx5/
	install -m 644 writely.conf $(HOME)/.local/share/fcitx5/addon/
	@echo "Installed for this user. Restart fcitx5 with: FCITX_ADDON_DIRS=$$HOME/.local/lib/fcitx5:$(LIBDIR)/fcitx5 fcitx5 -rd"

clean:
	rm -f libwritely.so writely.o

.PHONY: all install install-user clean
