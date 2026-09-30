#ifndef FILEVIEW_H
#define FILEVIEW_H

#include <QListView>
#include <QStringList>

class QMimeData;

/* A file list you can pick rows out of, and - once it has been told which folder it is showing - one
 * you can put rows into.
 *
 * The payload is `application/x-fam-paths`, one path per line, and it is deliberately *not* the
 * standard `text/uri-list`. A uri-list would be answered by every other application on the desktop, so
 * dragging a row onto a file window somewhere else would offer to move it under that program's rules:
 * no `fam_path_op`, no "never overwrite" check, no audit record. Two ways to move a file, only one of
 * them accounted for, is not a trade worth making. The private type refuses the same thing going the
 * other way - `dragEnterEvent` ignores anything that is not ours, so a drag started outside this
 * window is turned away at the border instead of half-understood.
 *
 * What a drop means is decided here and nowhere else: the folder this view owns is the destination,
 * the item keeps its name, Ctrl held means copy and no modifier means move. `proposedAction()` is not
 * consulted, because Qt picks a view's default drop action from its drag-drop mode (copy for a list)
 * and a move that quietly came back a copy would leave the original where it was and say nothing.
 * The drop position is not consulted either: within a Miller column the destination is the column's
 * folder whether the cursor is over a row, over a folder row, or over the empty space below.
 */
class FileView final : public QListView {
    Q_OBJECT

public:
    explicit FileView(QWidget *parent = nullptr);

    /* The one mime type this window speaks for drags, and the codec for both ends of it.
     * `FamListModel::mimeData` builds its payload through `mimeDataForPaths`, and the headless
     * transfer step decodes a payload through `pathsOf` before dropping it - so the bytes a real drag
     * carries and the bytes the drop handler reads come from the same two functions. */
    static const char *const kMime;
    static QMimeData *mimeDataForPaths(const QStringList &paths);
    /* The paths a payload carries; empty when it is not one of ours. */
    static QStringList pathsOf(const QMimeData *data);

    /* Naming a folder makes this view a target for drops into that folder. Leaving it empty - the
     * default - keeps the view a source only, which is the state the search-result list is in: its
     * rows come from however many folders the query reached, so there is no one directory a drop
     * could mean. */
    void setDropFolder(const QString &folder);
    QString dropFolder() const { return m_folder; }

    /* The relay one connect points at: the delegate reports a chip click by model index, and this
     * turns it into the view's `tagActivated` by reading the path off its own model. Called from
     * the two places a row delegate is installed (the column chain and the pane's results). */
    void relayTagClick(const QModelIndex &index, const QString &tag);

signals:
    /* A drop this view took: `folder` is the one it owns, `paths` what the drag carried, `copy`
     * whether Ctrl was held when it landed. */
    void dropped(const QString &folder, const QStringList &paths, bool copy);
    /* A tag chip on one of this view's rows was clicked. `path` is the row's own, resolved here
     * because the delegate knows the model index but not which model it came from. */
    void tagActivated(const QString &path, const QString &tag);

protected:
    void dragEnterEvent(QDragEnterEvent *event) override;
    void dragMoveEvent(QDragMoveEvent *event) override;
    void dropEvent(QDropEvent *event) override;

private:
    bool takes(const QMimeData *data) const;

    QString m_folder;
};

#endif /* FILEVIEW_H */
