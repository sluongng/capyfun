package greeter

import "testing"

func TestClient(t *testing.T) {
	if got := Connect("x").Addr(); got != "x" {
		t.Fatalf("Addr() = %q, want x", got)
	}
}
