package file

import (
	"fmt"
	"github.com/superboomer/metadata/exif/exiftag"

	"os"

	"github.com/dsoprea/go-exif/v3"
	// "golang.org/x/image/tiff"
)

// ImageMetadata 存储图像的元数据信息
type ImageMetadata struct {
	Path     string
	ExifData map[string]string
}

// ExtractExif 从图像文件中提取 EXIF 数据
func ExtractExif(filePath string) (*ImageMetadata, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return nil, fmt.Errorf("无法打开文件: %w", err)
	}
	defer file.Close()

	exifData, err := exif.Decode(file)
	if err != nil {
		return nil, fmt.Errorf("无法提取 EXIF 数据: %w", err)
	}

	imd := &ImageMetadata{
		Path:     filePath,
		ExifData: make(map[string]string),
	}

	ti := exifData.Root()
	err = ti.Walk(imd.collectExifData)
	if err != nil {
		return nil, fmt.Errorf("无法遍历 EXIF 数据: %w", err)
	}

	return imd, nil
}

// collectExifData 将 EXIF 数据收集到 ImageMetadata 结构中
func (imd *ImageMetadata) collectExifData(tagId exiftag.ExifTagId, value interface{}, index int) error {
	tagName, err := tagId.Name()
	if err != nil {
		return fmt.Errorf("无法获取标签名称: %w", err)
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
	case []float64:
		imd.ExifData[tagName] = fmt.Sprintf("%v", v) // 处理GPS坐标等数组类型
	default:
		imd.ExifData[tagName] = fmt.Sprintf("%v", v)
	}

	return nil
}

// ProcessRAW 处理 RAW 格式的图像
func ProcessRAW(filePath string) (*ImageMetadata, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return nil, fmt.Errorf("无法打开文件: %w", err)
	}
	defer file.Close()

	// 直接解析原始文件的EXIF（跳过TIFF转换）
	exifData, err := exif.Decode(file)
	if err != nil {
		return nil, fmt.Errorf("无法解析 EXIF 数据: %w", err)
	}

	imd := &ImageMetadata{
		Path:     filePath,
		ExifData: make(map[string]string),
	}

	// 替换RootIfd()为字段访问
	ti := exifData.RootIFD()
	err = ti.Walk(imd.collectExifData)
	if err != nil {
		return nil, fmt.Errorf("无法遍历 EXIF 数据: %w", err)
	}

	return imd, nil
}

// 示例用法
func ExampleExtractExif() {
	imd, err := ExtractExif("example.jpg")
	if err != nil {
		fmt.Println("错误:", err)
		return
	}

	fmt.Printf("文件 %s 的 EXIF 数据:\n", imd.Path)
	for key, value := range imd.ExifData {
		fmt.Printf("%s: %s\n", key, value)
	}
}

func ExampleProcessRAW() {
	imd, err := ProcessRAW("example.raw")
	if err != nil {
		fmt.Println("错误:", err)
		return
	}

	fmt.Printf("文件 %s 的 EXIF 数据:\n", imd.Path)
	for key, value := range imd.ExifData {
		fmt.Printf("%s: %s\n", key, value)
	}
}
