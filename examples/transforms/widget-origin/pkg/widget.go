// Package widget is a tiny demo library standing in for github.com/acme/widget,
// the upstream that //third_party/widget imports in this example.
package widget

import "acme.internal/log"

// New returns a greeting produced by the widget.
func New(name string) string {
	log.Debug("creating widget")
	return "widget:" + name
}
