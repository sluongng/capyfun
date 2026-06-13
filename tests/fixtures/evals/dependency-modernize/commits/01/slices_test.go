package slices

import "testing"

func TestFirst(t *testing.T) {
	if First([]int{1, 2, 3}) != 1 {
		t.Fatal("First wrong")
	}
}
