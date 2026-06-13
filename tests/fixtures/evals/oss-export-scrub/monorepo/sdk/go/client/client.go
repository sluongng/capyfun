// Package client is the acme SDK, developed inside the monorepo and published
// out to github.com/acme/sdk-go via `capyfun export`.
package client

// Version is the SDK version.
const Version = "1.0.0"

// BaseURL is the API endpoint. Internally the SDK targets acme's private control
// plane; the exported OSS build targets the public API. The markers below are
// rewritten on export (see ../SRC).
const BaseURL = "https://control.internal.acme.corp" // @--internal only--
// const BaseURL = "https://api.acme.dev" // @--OSS only--

// Hello returns the SDK greeting.
func Hello() string { return "hello from acme sdk-go" }
