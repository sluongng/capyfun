// Package client is the acme SDK, developed inside the monorepo and published
// out to github.com/acme/sdk-go via `capyfun export`.
package client

// Version is the SDK version.
const Version = "1.0.0"

// Hello returns the SDK greeting.
func Hello() string {
	return "hello from acme sdk-go"
}
