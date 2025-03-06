package tag

import (
	"fmt"
	"sync"
)

// PriorityManager manages the priority of tags for files
type PriorityManager struct {
	priorityMutex sync.RWMutex
	priorities    map[string]map[string]int // {filepath: {tag: priority}}
}

// NewPriorityManager creates a new PriorityManager instance
func NewPriorityManager() *PriorityManager {
	return &PriorityManager{
		priorities: make(map[string]map[string]int),
	}
}

// SetPriority sets the priority of a tag for a file
func (pm *PriorityManager) SetPriority(filepath string, tag string, priority int) {
	pm.priorityMutex.Lock()
	defer pm.priorityMutex.Unlock()

	if _, exists := pm.priorities[filepath]; !exists {
		pm.priorities[filepath] = make(map[string]int)
	}

	pm.priorities[filepath][tag] = priority
}

// GetPriority retrieves the priority of a tag for a file
func (pm *PriorityManager) GetPriority(filepath string, tag string) (int, bool) {
	pm.priorityMutex.RLock()
	defer pm.priorityMutex.RUnlock()

	if filePriorities, exists := pm.priorities[filepath]; exists {
		if priority, exists := filePriorities[tag]; exists {
			return priority, true
		}
	}
	return 0, false
}

// UpdatePriority updates the priority of a tag for a file
func (pm *PriorityManager) UpdatePriority(filepath string, tag string, priority int) {
	pm.SetPriority(filepath, tag, priority)
}

// Example usage
func ExamplePriorityManager() {
	pm := NewPriorityManager()
	pm.SetPriority("photo1.jpg", "landscape", 1)
	pm.SetPriority("photo1.jpg", "outdoor", 2)
	pm.SetPriority("photo2.jpg", "portrait", 1)
	pm.SetPriority("photo2.jpg", "indoor", 2)

	priority, exists := pm.GetPriority("photo1.jpg", "landscape")
	fmt.Println(priority, exists) // Output: 1 true

	priority, exists = pm.GetPriority("photo1.jpg", "nature")
	fmt.Println(priority, exists) // Output: 0 false

	pm.UpdatePriority("photo1.jpg", "landscape", 3)
	priority, exists = pm.GetPriority("photo1.jpg", "landscape")
	fmt.Println(priority, exists) // Output: 3 true
}
