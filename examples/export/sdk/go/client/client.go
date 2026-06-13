// Package client is the acme SDK, developed inside the monorepo and published
// out to github.com/acme/sdk-go via `capyfun export`.
package client

// Version is the SDK version.
const Version = "1.0.0"

// BaseURL is the API endpoint the SDK targets. Internally the SDK talks to
// acme's private control plane; the exported OSS build talks to the public API.
// The visibility markers below are rewritten on export (see ../SRC).
const BaseURL = "https://control.internal.acme.corp" // @--internal only--
// const BaseURL = "https://api.acme.dev" // @--OSS only--

// Hello returns the SDK greeting.
func Hello() string {
	return "hello from acme sdk-go"
}
