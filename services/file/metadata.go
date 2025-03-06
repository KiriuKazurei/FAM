package core

import (
	"bytes"
	"fmt"
	"io"
	"os"

	"github.com/dsoprea/go-exif/v3"
	"github.com/dsoprea/go-exif/v3/exifcommon"
	"github.com/dsoprea/go-exif/v3/exiftag"
	"golang.org/x/image/tiff"
)

// ImageMetadata holds metadata information for an image
type ImageMetadata struct {
	Path     string
	ExifData map[string]string
}

// ExtractExif extracts EXIF data from an image file
func ExtractExif(filePath string) (*ImageMetadata, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return nil, fmt.Errorf("failed to open file: %w", err)
	}
	defer file.Close()

	exifData, err := exif.SearchAndExtractExif(file)
	if err != nil {
		return nil, fmt.Errorf("failed to extract EXIF data: %w", err)
	}

	imd := &ImageMetadata{
		Path:     filePath,
		ExifData: make(map[string]string),
	}

	ti := exifData.RootIfd()
	err = ti.Walk(imd.collectExifData)
	if err != nil {
		return nil, fmt.Errorf("failed to walk EXIF data: %w", err)
	}

	return imd, nil
}

// collectExifData collects EXIF data into the ImageMetadata struct
func (imd *ImageMetadata) collectExifData(tagId exiftag.ExifTagId, value interface{}, index int) error {
	tagName, err := tagId.Name()
	if err != nil {
		return fmt.Errorf("failed to get tag name: %w", err)
	}

	switch v := value.(type) {
	case []byte:
		imd.ExifData[tagName] = string(v)
	case string:
		imd.ExifData[tagName] = v
	case int:
		imd.ExifData[tagName] = fmt.Sprintf("%d", v)
	case uint16:
		imd.ExifData[tagName] = fmt.Sprintf("%d", v)
	case uint32:
		imd.ExifData[tagName] = fmt.Sprintf("%d", v)
	case float64:
		imd.ExifData[tagName] = fmt.Sprintf("%f", v)
	default:
		imd.ExifData[tagName] = fmt.Sprintf("%v", v)
	}

	return nil
}

// ProcessRAW processes RAW format images
func ProcessRAW(filePath string) (*ImageMetadata, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return nil, fmt.Errorf("failed to open file: %w", err)
	}
	defer file.Close()

	tiffReader, err := tiff.Decode(file)
	if err != nil {
		return nil, fmt.Errorf("failed to decode TIFF: %w", err)
	}

	buf := new(bytes.Buffer)
	err = tiff.Encode(buf, tiffReader, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to encode TIFF: %w", err)
	}

	imd := &ImageMetadata{
		Path:     filePath,
		ExifData: make(map[string]string),
	}

	// Extract EXIF data from the TIFF buffer
	exifData, err := exif.SearchAndExtractExif(bytes.NewReader(buf.Bytes()))
	if err != nil {
		return nil, fmt.Errorf("failed to extract EXIF data from TIFF: %w", err)
	}

	ti := exifData.RootIfd()
	err = ti.Walk(imd.collectExifData)
	if err != nil {
		return nil, fmt.Errorf("failed to walk EXIF data: %w", err)
	}

	return imd, nil
}

// Example usage
func ExampleExtractExif() {
	imd, err := ExtractExif("example.jpg")
	if err != nil {
		fmt.Println("Error:", err)
		return
	}

	fmt.Printf("EXIF Data for %s:\n", imd.Path)
	for key, value := range imd.ExifData {
		fmt.Printf("%s: %s\n", key, value)
	}
}

func ExampleProcessRAW() {
	imd, err := ProcessRAW("example.raw")
	if err != nil {
		fmt.Println("Error:", err)
		return
	}

	fmt.Printf("EXIF Data for %s:\n", imd.Path)
	for key, value := range imd.ExifData {
		fmt.Printf("%s: %s\n", key, value)
	}
}