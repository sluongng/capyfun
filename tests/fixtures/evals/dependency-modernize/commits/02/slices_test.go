package slices

import "testing"

func TestFirst(t *testing.T) {
	if First([]int{1, 2, 3}) != 1 {
		t.Fatal("First wrong")
	}
}

// New upstream contract: Last must return the final element.
func TestLast(t *testing.T) {
	if Last([]int{1, 2, 3}) != 3 {
		t.Fatalf("Last = %d, want 3", Last([]int{1, 2, 3}))
	}
}
