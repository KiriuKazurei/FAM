#ifndef TAGDIALOG_H
#define TAGDIALOG_H

#include <QDialog>
#include <QString>

class FamBridge;
class QLabel;
class QLineEdit;
class QListWidget;

/* The tags on one file: what they are, who put them there, and a way to take one back off.
 *
 * This dialog exists because the shell could previously only *add* a tag. `fam_tag_list` and
 * `fam_tag_remove` are both on the ABI and both wrapped in `FamBridge`, and nothing in the
 * interface called either - so a tag went in and never came out, and the file's own tags were
 * invisible even to the person who had just typed one.
 *
 * A tag carries where it came from (`manual` or `auto`) and a priority, both of which the store
 * orders by and neither of which the old prompt showed. They are listed here because a tag the
 * indexer wrote is not something a person should be asked to silently delete, and it is the reason
 * removal names the row it removes rather than taking the whole file's tags. */
class TagDialog final : public QDialog {
    Q_OBJECT

public:
    TagDialog(FamBridge *bridge, const QString &path, QWidget *parent = nullptr);

    /* The three operations, callable without any of the widgets being shown. A person's click
     * reaches them through the slots below; the headless step calls them directly, because a modal
     * dialog is exactly the thing a run with no hands cannot drive. */

    /* What the file carries right now, one entry per tag *name*, in the store's display order. */
    QStringList tagNames() const;
    bool addTag(const QString &tag, QString *error);
    bool removeTag(const QString &name, QString *error);
    /* Re-reads the store and refreshes the list. */
    void reload();

    /* True once anything has been added or removed, so the window can say so on the status line
     * rather than guess from a dialog closing. */
    bool changed() const { return m_changed; }

private slots:
    void add();
    void removeSelected();

private:
    void showList();

    FamBridge *m_bridge;
    QString m_path;
    /* The list's rows, kept beside the widget rather than read back out of it: `m_names` is what the
     * operations address and what a headless run asserts on, and neither depends on the dialog ever
     * having been shown. `m_display` is the same rows with the sources spelled out. */
    QStringList m_names;
    QStringList m_display;
    QListWidget *m_list = nullptr;
    QLineEdit *m_entry = nullptr;
    QLabel *m_note = nullptr;
    bool m_changed = false;
};

#endif /* TAGDIALOG_H */
