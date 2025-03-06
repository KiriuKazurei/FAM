package tag

import (
	"fmt"
	"sync"
)

// TagManager manages file tags, including both manual and AI-generated tags
type TagManager struct {
	tagsMutex sync.RWMutex
	tags      map[string]map[string]bool // {filepath: {tag: isManual}}
}

// NewTagManager creates a new TagManager instance
func NewTagManager() *TagManager {
	return &TagManager{
		tags: make(map[string]map[string]bool),
	}
}

// AddTags adds tags to a file, with a flag indicating if they are manual
func (tm *TagManager) AddTags(filepath string, tags []string, isManual bool) {
	tm.tagsMutex.Lock()
	defer tm.tagsMutex.Unlock()

	if _, exists := tm.tags[filepath]; !exists {
		tm.tags[filepath] = make(map[string]bool)
	}

	for _, tag := range tags {
		tm.tags[filepath][tag] = isManual
	}
}

// GetTags retrieves all tags for a file, prioritizing manual tags
func (tm *TagManager) GetTags(filepath string) []string {
	tm.tagsMutex.RLock()
	defer tm.tagsMutex.RUnlock()

	var tags []string
	if fileTags, exists := tm.tags[filepath]; exists {
		for tag, isManual := range fileTags {
			if isManual {
				tags = append(tags, tag)
			}
		}
		for tag, isManual := range fileTags {
			if !isManual {
				tags = append(tags, tag)
			}
		}
	}
	return tags
}

// SearchByTags searches for files by tags, prioritizing manual tags
func (tm *TagManager) SearchByTags(searchTags []string) []string {
	tm.tagsMutex.RLock()
	defer tm.tagsMutex.RUnlock()

	var results []string
	fileLoop:
	for filepath, fileTags := range tm.tags {
		for _, searchTag := range searchTags {
			found := false
			for tag, isManual := range fileTags {
				if tag == searchTag {
					if isManual {
						results = append(results, filepath)
						continue fileLoop
					}
					found = true
				}
			}
			if found {
				results = append(results, filepath)
			}
		}
	}
	return results
}

// Example usage
func ExampleTagManager() {
	tm := NewTagManager()
	tm.AddTags("photo1.jpg", []string{"landscape", "outdoor"}, true)
	tm.AddTags("photo1.jpg", []string{"nature"}, false)
	tm.AddTags("photo2.jpg", []string{"portrait", "indoor"}, true)
	tm.AddTags("photo2.jpg", []string{"people"}, false)

	fmt.Println(tm.GetTags("photo1.jpg")) // Output: [landscape outdoor nature]
	fmt.Println(tm.GetTags("photo2.jpg")) // Output: [portrait indoor people]

	results := tm.SearchByTags([]string{"landscape", "people"})
	fmt.Println(results) // Output: [photo1.jpg photo2.jpg]
}