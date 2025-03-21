package imageutil

import "C"
import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"image"
	"image/jpeg"
	"os"
	"path/filepath"
	"sync"
	"time"
	"unsafe"
)

package imageutil

import (
"errors"
"image"
"image/jpeg"
"os"
"path/filepath"
"sync"
"time"

"github.com/disintegration/imaging"
)

// 缩略图生成配置
type ThumbnailConfig struct {
	MaxDimension  int     // 最大边长（像素）
	JPEGQuality   int     // JPEG压缩质量（1-100）
	CacheDir      string  // 缓存目录路径
	WorkerCount   int     // 并行工作协程数
	CacheDuration int64   // 缓存有效期（秒）
}

var DefaultThumbnailConfig = &ThumbnailConfig{
	MaxDimension:  512,
	JPEGQuality:   85,
	CacheDir:      ".thumbcache",
	WorkerCount:   4,
	CacheDuration: 604800, // 7天
}

type ThumbnailGenerator struct {
	config *ThumbnailConfig
	taskCh chan *thumbnailTask
	wg     sync.WaitGroup
}

type thumbnailTask struct {
	srcPath    string
	resultChan chan *thumbnailResult
}

type thumbnailResult struct {
	thumbPath string
	err       error
}

// 初始化缩略图生成器
func NewThumbnailGenerator(cfg *ThumbnailConfig) *ThumbnailGenerator {
	if cfg == nil {
		cfg = DefaultThumbnailConfig
	}

	// 创建缓存目录
	if err := os.MkdirAll(cfg.CacheDir, 0755); err != nil {
		panic(err)
	}

	gen := &ThumbnailGenerator{
		config: cfg,
		taskCh: make(chan *thumbnailTask, 100),
	}

	// 启动工作池
	for i := 0; i < cfg.WorkerCount; i++ {
		go gen.worker()
	}

	return gen
}

// 生成或获取缩略图（异步接口）
func (g *ThumbnailGenerator) GetThumbnailAsync(srcPath string) <-chan *thumbnailResult {
	resultChan := make(chan *thumbnailResult, 1)
	task := &thumbnailTask{
		srcPath:    srcPath,
		resultChan: resultChan,
	}

	g.wg.Add(1)
	go func() {
		defer g.wg.Done()
		g.taskCh <- task
	}()

	return resultChan
}

// 工作协程处理逻辑
func (g *ThumbnailGenerator) worker() {
	for task := range g.taskCh {
		result := &thumbnailResult{}

		// 优先检查缓存
		cachePath := g.getCachePath(task.srcPath)
		if g.isCacheValid(cachePath) {
			result.thumbPath = cachePath
		} else {
			// 实际生成逻辑
			var img image.Image
			var err error

			if isRawFile(task.srcPath) {
				img, err = generateRawThumbnail(task.srcPath, g.config.MaxDimension)
			} else {
				img, err = generateStandardThumbnail(task.srcPath, g.config.MaxDimension)
			}

			if err == nil {
				err = saveThumbnail(img, cachePath, g.config.JPEGQuality)
			}

			if err != nil {
				result.err = err
			} else {
				result.thumbPath = cachePath
			}
		}

		task.resultChan <- result
		close(task.resultChan)
	}
}

// RAW格式缩略图生成（使用libraw）
func generateRawThumbnail(path string, maxDim int) (image.Image, error) {
	/*
		#include <libraw.h>
	*/
	processor := C.libraw_init(C.LIBRAW_OPIONS_NO_DATAERR_CALLBACK)
	defer C.libraw_close(processor)

	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))

	if ret := C.libraw_open_file(processor, cPath); ret != 0 {
		return nil, errors.New("failed to open raw file")
	}

	if ret := C.libraw_unpack_thumb(processor); ret != 0 {
		return nil, errors.New("failed to unpack thumbnail")
	}

	thumb := processor.thumbnail
	if thumb.tformat == C.LIBRAW_THUMBNAIL_JPEG {
		// 直接提取JPEG预览
		data := C.GoBytes(unsafe.Pointer(thumb.thumb), C.int(thumb.tlength))
		return imaging.Decode(bytes.NewReader(data))
	}

	// 降级处理：解码RAW数据生成缩略图
	return nil, errors.New("unsupported raw thumbnail format")
}

// 标准图像缩略图生成
func generateStandardThumbnail(path string, maxDim int) (image.Image, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()

	img, _, err := image.Decode(file)
	if err != nil {
		return nil, err
	}

	return imaging.Fit(img, maxDim, maxDim, imaging.Lanczos), nil
}

// 保存缩略图到缓存
func saveThumbnail(img image.Image, path string, quality int) error {
	file, err := os.Create(path)
	if err != nil {
		return err
	}
	defer file.Close()

	return jpeg.Encode(file, img, &jpeg.Options{Quality: quality})
}

// 获取缓存路径
func (g *ThumbnailGenerator) getCachePath(srcPath string) string {
	hash := sha256.Sum256([]byte(srcPath))
	filename := hex.EncodeToString(hash[:]) + ".jpg"
	return filepath.Join(g.config.CacheDir, filename)
}

// 检查缓存有效性
func (g *ThumbnailGenerator) isCacheValid(cachePath string) bool {
	info, err := os.Stat(cachePath)
	if os.IsNotExist(err) {
		return false
	}

	// 检查缓存时间
	return time.Now().Unix()-info.ModTime().Unix() < g.config.CacheDuration
}

// 清理过期缓存
func (g *ThumbnailGenerator) CleanupCache() error {
	return filepath.Walk(g.config.CacheDir, func(path string, info os.FileInfo, err error) error {
		if time.Since(info.ModTime()) > time.Duration(g.config.CacheDuration)*time.Second {
			return os.Remove(path)
		}
		return nil
	})
}





// 用法
//func main() {
//	// 初始化生成器
//	cfg := &imageutil.ThumbnailConfig{
//		MaxDimension: 256,
//		JPEGQuality:  90,
//		CacheDir:     ".cache",
//		WorkerCount:  2,
//	}
//	gen := imageutil.NewThumbnailGenerator(cfg)
//	defer gen.CleanupCache()
//
//	// 扫描图片文件
//	images, _ := imageutil.ScanImagePaths("./photos")
//
//	// 异步生成缩略图
//	results := make([]<-chan *imageutil.thumbnailResult, 0)
//	for _, img := range images {
//		results = append(results, gen.GetThumbnailAsync(img.Path))
//	}
//
//	// 等待结果
//	for _, ch := range results {
//		res := <-ch
//		if res.err != nil {
//			fmt.Printf("生成失败: %v\n", res.err)
//		} else {
//			fmt.Printf("缩略图生成: %s\n", res.thumbPath)
//		}
//	}
//}
