package widget

import "testing"

func TestNew(t *testing.T) {
	if got := New("a"); got != "widget:a" {
		t.Fatalf("New(\"a\") = %q, want %q", got, "widget:a")
	}
}
