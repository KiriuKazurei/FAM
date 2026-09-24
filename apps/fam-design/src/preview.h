#ifndef PREVIEW_WINDOW_H
#define PREVIEW_WINDOW_H

#include <QVector>
#include <QWidget>

class QAction;
class QComboBox;
class QLabel;
class QLineEdit;
class QListWidget;
class QListView;
class PreviewModel;
class QPushButton;
class QStackedWidget;

/* The Ant Design 5 treatment of this product's main window: a drive rail, two Miller columns of real
 * folders, and the toolbar the shell has - restyled, not re-plumbed.
 *
 * It is deliberately a separate program. It links no core, no ABI, no `fam-svc-*`; it reads folders
 * with `QDir` and forgets about them on the next keystroke. That is what makes it safe to look at the
 * design without the design reaching into the product: the shell in `apps/fam-shell` keeps its own
 * appearance, and nothing here can change what that one paints.
 *
 * Everything on screen does something real, because a design sample whose buttons are decoration
 * answers no question: the search box filters, the two selects filter and re-order, a folder in the
 * left column becomes the right column, "Open folder…" opens a folder, and the corner button swaps
 * the light and dark scheme.
 *
 * What is not here: icons from the shell (that is `shell32`, and the row falls back to a line glyph),
 * watches, pagination, tags, and the polling the product does. Those are behaviours, not looks, and a
 * styling decision about them belongs with the shell. */
class PreviewWindow final : public QWidget {
    Q_OBJECT

public:
    explicit PreviewWindow(const QString &startDirectory, QWidget *parent = nullptr);

private:
    /* The two columns, top to bottom: a header strip, then the list or the empty state. */
    QWidget *buildColumn(int index);
    /* Re-reads both columns through the current needle, bucket and order. */
    void refresh();
    /* Points the right-hand column at `path` and hides every column deeper than it. */
    void descend(int level, const QString &path);
    void updateTitles();
    void updateStatus();
    void applySearchGlyph();
    void toggleScheme();
    void browseForFolder();

    /* How many columns the design shows. Two is what a 960px window fits at the column's ideal width,
     * which is the number the product's own chain cap is chosen around. */
    static constexpr int kColumns = 2;

    QLineEdit *m_search = nullptr;
    QComboBox *m_kind = nullptr;
    QComboBox *m_sort = nullptr;
    QPushButton *m_themeButton = nullptr;
    QAction *m_searchGlyph = nullptr;
    QListWidget *m_rail = nullptr;
    QLabel *m_status = nullptr;
    QVector<QWidget *> m_cards;
    QVector<QLabel *> m_titles;
    QVector<QPushButton *> m_headers;
    QVector<QListView *> m_views;
    QVector<PreviewModel *> m_models;
    QVector<QStackedWidget *> m_pages;
    /* The folder each column is showing, by index. Empty means that column is not on screen. */
    QVector<QString> m_shown;
};

#endif /* PREVIEW_WINDOW_H */
