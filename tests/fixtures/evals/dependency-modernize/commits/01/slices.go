// Package slices has small generic helpers.
package slices

// First returns the initial element of s.
func First[T any](s []T) T { return s[0] }
